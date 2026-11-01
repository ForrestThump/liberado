//! The request-time guard: who may call the API, and which calls need the owner.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chat_client_contract::ApiError;
use liberado_config::ApiConfig;
use liberado_outbox::Outbox;

use super::challenges::ChallengeStore;
use super::rate_limit::AuthRateLimiter;
use super::sessions::{CSRF_HEADER, SESSION_COOKIE, SessionStore, cookie_value};
use super::tokens::{ApiAuth, Principal};
use super::webauthn_config::WebAuthnState;

/// Everything the auth layer needs, held apart from `AppState` so a handler cannot reach a
/// token by going through the state it already has.
pub struct ApiGuard {
    pub auth: ApiAuth,
    pub sessions: SessionStore,
    /// Exact browser origins allowed to call the API cross-origin (`[api] allowed_origins`).
    pub allowed_origins: Vec<String>,
    pub webauthn: Option<WebAuthnState>,
    pub challenges: ChallengeStore,
    pub rate_limiter: AuthRateLimiter,
}

impl ApiGuard {
    pub fn new(
        auth: ApiAuth,
        api: &ApiConfig,
        outbox: Arc<Outbox>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let webauthn = match &api.webauthn {
            Some(cfg) => Some(super::webauthn_config::build_webauthn(cfg)?),
            None => None,
        };
        Ok(Self {
            auth,
            sessions: SessionStore::new(outbox),
            allowed_origins: api.allowed_origins.clone(),
            webauthn,
            challenges: ChallengeStore::default(),
            rate_limiter: AuthRateLimiter::default(),
        })
    }

    /// A guard with no credentials, for tests about something other than authentication.
    #[cfg(test)]
    pub fn open_for_test() -> Arc<Self> {
        let dir = tempfile::tempdir().expect("tempdir");
        let outbox = Arc::new(Outbox::open(&dir.path().join("liberado.db")).expect("outbox"));
        Arc::new(Self::new(ApiAuth::default(), &ApiConfig::default(), outbox).expect("open guard"))
    }

    #[cfg(test)]
    pub fn with_auth(auth: ApiAuth) -> Arc<Self> {
        Self::with_api(auth, ApiConfig::default())
    }

    #[cfg(test)]
    pub fn with_api(auth: ApiAuth, api: ApiConfig) -> Arc<Self> {
        let dir = tempfile::tempdir().expect("tempdir");
        let outbox = Arc::new(Outbox::open(&dir.path().join("liberado.db")).expect("outbox"));
        Arc::new(Self::new(auth, &api, outbox).expect("guard"))
    }
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(ApiError {
            error: message.to_string(),
        }),
    )
        .into_response()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn session_id(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie_value(raw, SESSION_COOKIE)
}

fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn identify(guard: &ApiGuard, headers: &HeaderMap) -> Option<Principal> {
    if let Some(token) = bearer(headers) {
        return guard.auth.authenticate_bearer(token);
    }
    let id = session_id(headers)?;
    guard.sessions.touch(id).map(|live| Principal::Human {
        account_id: live.account_id as u64,
        via_cookie: true,
        session_method: Some(live.method),
    })
}

fn cookie_request_is_same_origin(
    guard: &ApiGuard,
    headers: &HeaderMap,
) -> Result<(), &'static str> {
    let Some(id) = session_id(headers) else {
        return Err("no session");
    };
    let Some(expected) = guard.sessions.touch(id).map(|l| l.csrf) else {
        return Err("no session");
    };
    let presented = headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if presented != expected {
        return Err(
            "missing or wrong X-Liberado-CSRF header; a browser session must echo the CSRF \
             token issued at login",
        );
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && !origin_is_allowed(guard, headers, origin)
    {
        return Err("request Origin is not this daemon's own origin");
    }
    Ok(())
}

fn origin_is_allowed(guard: &ApiGuard, headers: &HeaderMap, origin: &str) -> bool {
    if guard.allowed_origins.iter().any(|o| o == origin) {
        return true;
    }
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    origin
        .split_once("://")
        .is_some_and(|(_, authority)| authority == host)
}

pub async fn require_credential(
    State(guard): State<Arc<ApiGuard>>,
    mut request: Request,
    next: Next,
) -> Response {
    if guard.auth.is_open() {
        request.extensions_mut().insert(Principal::Human {
            account_id: 1,
            via_cookie: false,
            session_method: None,
        });
        return next.run(request).await;
    }

    let Some(principal) = identify(&guard, request.headers()) else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthenticated: send `Authorization: Bearer <token>`, or sign in at \
             POST /api/auth/login to get a session cookie",
        );
    };

    if matches!(
        principal,
        Principal::Human {
            via_cookie: true,
            ..
        }
    ) && is_mutating(request.method())
        && let Err(why) = cookie_request_is_same_origin(&guard, request.headers())
    {
        return refuse(StatusCode::FORBIDDEN, why);
    }

    tracing::debug!(principal = principal.label(), path = %request.uri().path(), "api request");
    request.extensions_mut().insert(principal);
    next.run(request).await
}

pub async fn require_owner(request: Request, next: Next) -> Response {
    match request.extensions().get::<Principal>() {
        Some(p) if p.is_human() => next.run(request).await,
        Some(Principal::Client { name }) => refuse(
            StatusCode::FORBIDDEN,
            &format!(
                "client '{name}' may not call owner-only endpoints; this route needs the human \
                 session credential"
            ),
        ),
        _ => refuse(
            StatusCode::UNAUTHORIZED,
            "owner-only endpoint reached without an authenticated principal",
        ),
    }
}

pub fn principal_from_request(request: &Request) -> Option<Principal> {
    request.extensions().get::<Principal>().cloned()
}

pub fn can_register_passkeys(guard: &ApiGuard, headers: &HeaderMap) -> bool {
    if let Some(token) = bearer(headers) {
        return guard.auth.is_human_token(token);
    }
    let Some(id) = session_id(headers) else {
        return false;
    };
    let Some(live) = guard.sessions.touch(id) else {
        return false;
    };
    live.method == "token"
}

pub fn passkey_session_forbidden_message() -> &'static str {
    "passkey sessions cannot register new passkeys; sign in with the emergency token first"
}

#[cfg(test)]
#[path = "guard_tests.rs"]
mod tests;
