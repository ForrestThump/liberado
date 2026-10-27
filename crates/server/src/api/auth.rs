//! `POST /api/auth/login`, `GET /api/auth/session`, `POST /api/auth/logout`, `GET /api/health`.
//!
//! How Forrest signs in to the WebUI: paste the human token once, get a cookie. The browser
//! never stores the token, and page script can never read the cookie that replaces it.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chat_client_contract::ApiError;
use serde::{Deserialize, Serialize};

use crate::auth::guard::ApiGuard;
use crate::auth::sessions::{CSRF_COOKIE, SESSION_COOKIE, cookie_value};

#[derive(Deserialize)]
pub struct LoginRequest {
    pub token: String,
}

/// What the browser needs to know, and nothing it could not already see.
#[derive(Serialize)]
pub struct SessionResponse {
    /// `true` when this request carried a live owner session.
    pub authenticated: bool,
    /// `true` when an owner credential is configured, so signing in is possible at all. A
    /// loopback daemon with no `[api]` table reports `false`, and the WebUI shows no sign-in
    /// box because there is nothing to type.
    pub login_required: bool,
    /// The token to echo in `X-Liberado-CSRF` on mutating requests. Present only for a live
    /// session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csrf: Option<String>,
}

/// Minimal liveness, deliberately unauthenticated and deliberately contentless.
///
/// The Compose healthcheck and any uptime monitor need a route that answers before anyone has
/// signed in. `GET /api/status` used to be that route; it reports the vault path, the active
/// model, and every connected tool, which is a reconnaissance gift to an unauthenticated
/// caller. This says only that the process is up.
pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true }))
}

/// Exchange the human token for a session cookie.
///
/// A wrong token is a flat 401 that does not say whether a credential is configured, and the
/// comparison runs in constant time inside [`crate::auth::ApiAuth`].
pub async fn login(State(guard): State<Arc<ApiGuard>>, Json(body): Json<LoginRequest>) -> Response {
    if !guard.auth.has_human_credential() {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "no human credential is configured; set [api] human in topology.toml"
                    .to_string(),
            }),
        )
            .into_response();
    }
    if !guard.auth.is_human_token(body.token.trim()) {
        tracing::warn!("rejected an API sign-in attempt with a token that did not match");
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "that is not the human token".to_string(),
            }),
        )
            .into_response();
    }

    let (session_id, csrf) = guard.sessions.create();
    let mut headers = HeaderMap::new();
    append_cookie(
        &mut headers,
        &format!("{SESSION_COOKIE}={session_id}; HttpOnly; SameSite=Strict; Path=/"),
    );
    // Readable on purpose: the WebUI reads it back after a reload so a refresh does not need
    // another round trip before it can send a mutating request. It authenticates nothing — the
    // `HttpOnly` cookie above does that — so a script that reads it has gained nothing.
    append_cookie(
        &mut headers,
        &format!("{CSRF_COOKIE}={csrf}; SameSite=Strict; Path=/"),
    );
    (
        headers,
        Json(SessionResponse {
            authenticated: true,
            login_required: true,
            csrf: Some(csrf),
        }),
    )
        .into_response()
}

/// Whether this browser is signed in, and the CSRF token if so.
pub async fn session(State(guard): State<Arc<ApiGuard>>, headers: HeaderMap) -> Response {
    let csrf = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| cookie_value(raw, SESSION_COOKIE))
        .and_then(|id| guard.sessions.touch(id));
    Json(SessionResponse {
        authenticated: csrf.is_some(),
        login_required: !guard.auth.is_open(),
        csrf,
    })
    .into_response()
}

/// Revoke this session and clear both cookies.
pub async fn logout(State(guard): State<Arc<ApiGuard>>, headers: HeaderMap) -> Response {
    if let Some(id) = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| cookie_value(raw, SESSION_COOKIE))
    {
        guard.sessions.revoke(id);
    }
    let mut out = HeaderMap::new();
    append_cookie(
        &mut out,
        &format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
    );
    append_cookie(
        &mut out,
        &format!("{CSRF_COOKIE}=; SameSite=Strict; Path=/; Max-Age=0"),
    );
    (
        out,
        Json(SessionResponse {
            authenticated: false,
            login_required: !guard.auth.is_open(),
            csrf: None,
        }),
    )
        .into_response()
}

/// Append one `Set-Cookie`. A malformed value cannot occur here — both are hex and fixed
/// literals — so a header that will not build is dropped rather than given an error path no
/// test could reach.
fn append_cookie(headers: &mut HeaderMap, value: &str) {
    if let Ok(parsed) = value.parse() {
        headers.append(header::SET_COOKIE, parsed);
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
