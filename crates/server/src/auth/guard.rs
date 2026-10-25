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

use super::sessions::{CSRF_HEADER, SESSION_COOKIE, SessionStore, cookie_value};
use super::tokens::{ApiAuth, Principal};

/// Everything the auth layer needs, held apart from `AppState` so a handler cannot reach a
/// token by going through the state it already has.
pub struct ApiGuard {
    pub auth: ApiAuth,
    pub sessions: SessionStore,
    /// Exact browser origins allowed to call the API cross-origin (`[api] allowed_origins`).
    pub allowed_origins: Vec<String>,
}

impl ApiGuard {
    pub fn new(auth: ApiAuth, allowed_origins: Vec<String>) -> Self {
        Self {
            auth,
            sessions: SessionStore::new(),
            allowed_origins,
        }
    }

    /// A guard with no credentials, for tests about something other than authentication.
    ///
    /// It behaves exactly as a loopback daemon with no `[api]` table does, so a test that
    /// uses it is still exercising a real configuration rather than a bypass.
    #[cfg(test)]
    pub fn open_for_test() -> Arc<Self> {
        Arc::new(Self::new(ApiAuth::default(), Vec::new()))
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

/// The bearer token on this request, if it carries a well-formed `Authorization` header.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn session_id(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie_value(raw, SESSION_COOKIE)
}

/// Methods that can change something, and therefore need the CSRF check when the credential
/// is a cookie the browser attaches on its own.
fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Decide who is calling, from a bearer token or a session cookie.
fn identify(guard: &ApiGuard, headers: &HeaderMap) -> Option<Principal> {
    if let Some(token) = bearer(headers) {
        return guard.auth.authenticate_bearer(token);
    }
    let id = session_id(headers)?;
    guard
        .sessions
        .touch(id)
        .map(|_| Principal::Human { via_cookie: true })
}

/// Whether a cookie-authenticated mutating request proved it came from our own page.
///
/// Two independent checks, because they fail in different places. `SameSite=Strict` already
/// stops the browser sending the cookie cross-site, so reaching here cross-site should be
/// impossible; the CSRF token and the `Origin` check are what catch the case where it is not —
/// an old browser, a same-site subdomain, or a future change that relaxes the cookie.
fn cookie_request_is_same_origin(
    guard: &ApiGuard,
    headers: &HeaderMap,
) -> Result<(), &'static str> {
    let Some(id) = session_id(headers) else {
        return Err("no session");
    };
    let Some(expected) = guard.sessions.touch(id) else {
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

/// An `Origin` is ours when it names the host the request was addressed to, or when the
/// operator listed it in `[api] allowed_origins`.
fn origin_is_allowed(guard: &ApiGuard, headers: &HeaderMap, origin: &str) -> bool {
    if guard.allowed_origins.iter().any(|o| o == origin) {
        return true;
    }
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    // Compare authorities: `http://127.0.0.1:4201` against `127.0.0.1:4201`. The scheme is not
    // compared because the daemon serves plain HTTP on the tailnet and a reverse proxy may
    // front it with TLS without rewriting `Host`.
    origin
        .split_once("://")
        .is_some_and(|(_, authority)| authority == host)
}

/// Require a credential on every API route this layer covers.
///
/// Applied with `route_layer`, so it runs only for matched API routes and leaves the static
/// WebUI bundle reachable — the sign-in page has to load before there is anything to sign in
/// with.
pub async fn require_credential(
    State(guard): State<Arc<ApiGuard>>,
    mut request: Request,
    next: Next,
) -> Response {
    if guard.auth.is_open() {
        // No credential configured at all. The daemon only reaches this state on a loopback
        // bind — `ensure_bind_is_credentialed` refuses any other — so the operating system is
        // the boundary and demanding a token nobody has would lock the owner out of his own
        // box.
        request
            .extensions_mut()
            .insert(Principal::Human { via_cookie: false });
        return next.run(request).await;
    }

    let Some(principal) = identify(&guard, request.headers()) else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthenticated: send `Authorization: Bearer <token>`, or sign in at \
             POST /api/auth/login to get a session cookie",
        );
    };

    if matches!(principal, Principal::Human { via_cookie: true })
        && is_mutating(request.method())
        && let Err(why) = cookie_request_is_same_origin(&guard, request.headers())
    {
        return refuse(StatusCode::FORBIDDEN, why);
    }

    tracing::debug!(principal = principal.label(), path = %request.uri().path(), "api request");
    request.extensions_mut().insert(principal);
    next.run(request).await
}

/// Require the owner. Layered onto the routes that change authority rather than use it.
///
/// A client token authenticates; it does not make its holder Forrest. The case this exists for
/// is concrete: a coding run with a token in its environment reaching
/// `POST /api/approvals/{id}/resolve` and approving its own write.
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
        // No extension means this route was mounted outside `require_credential`. Refusing is
        // the only safe reading of a wiring mistake on an owner-only route.
        _ => refuse(
            StatusCode::UNAUTHORIZED,
            "owner-only endpoint reached without an authenticated principal",
        ),
    }
}

#[cfg(test)]
#[path = "guard_tests.rs"]
mod tests;
