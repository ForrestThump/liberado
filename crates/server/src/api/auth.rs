//! `POST /api/auth/login`, `GET /api/auth/session`, `POST /api/auth/logout`, passkeys, `GET /api/health`.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chat_client_contract::ApiError;
use serde::{Deserialize, Serialize};

use crate::auth::client_ip;
use crate::auth::cookies::{clear_cookie, session_max_age, set_session_cookies};
use crate::auth::guard::ApiGuard;
use crate::auth::sessions::{CSRF_COOKIE, SESSION_COOKIE, cookie_value};
use crate::auth::tokens::Principal;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub token: String,
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub authenticated: bool,
    pub login_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csrf: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub passkeys_enabled: bool,
    pub passkey_count: i64,
    pub can_register: bool,
}

pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true }))
}

pub async fn login(
    State(guard): State<Arc<ApiGuard>>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    let ip = client_ip::client_ip(&headers, None);
    if !guard.auth.has_human_credential() {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "no human credential is configured".to_string(),
            }),
        )
            .into_response();
    }
    if !guard.auth.is_human_token(body.token.trim()) {
        tracing::warn!(client = %ip, "rejected API sign-in attempt");
        if guard.rate_limiter.record_failure(&ip) {
            return rate_limited(&guard);
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "that is not the human token".to_string(),
            }),
        )
            .into_response();
    }
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    let (session_id, csrf) = match guard.sessions.create(1, "token", ua) {
        Ok(v) => v,
        Err(_) => return internal_error("could not create session"),
    };
    let live = match guard.sessions.touch(&session_id) {
        Some(l) => l,
        None => return internal_error("could not create session"),
    };
    let max_age = session_max_age(&live.expires_at);
    let mut out = HeaderMap::new();
    set_session_cookies(&mut out, &session_id, &csrf, max_age);
    (
        out,
        Json(session_body(
            &guard,
            Some(csrf.clone()),
            Some("token".into()),
            true,
        )),
    )
        .into_response()
}

pub async fn session(State(guard): State<Arc<ApiGuard>>, headers: HeaderMap) -> Response {
    let live = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| cookie_value(raw, SESSION_COOKIE))
        .and_then(|id| guard.sessions.touch(id));
    let authenticated = live.is_some();
    let csrf = live.as_ref().map(|l| l.csrf.clone());
    let method = live.map(|l| l.method);
    Json(session_body(&guard, csrf, method, authenticated)).into_response()
}

pub async fn logout(State(guard): State<Arc<ApiGuard>>, headers: HeaderMap) -> Response {
    if let Some(id) = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| cookie_value(raw, SESSION_COOKIE))
    {
        guard.sessions.revoke(id);
    }
    let mut out = HeaderMap::new();
    clear_cookie(&mut out, SESSION_COOKIE);
    clear_cookie(&mut out, CSRF_COOKIE);
    (out, Json(session_body(&guard, None, None, false))).into_response()
}

pub async fn list_passkeys(
    State(guard): State<Arc<ApiGuard>>,
    axum::Extension(principal): axum::Extension<Principal>,
) -> Response {
    if !principal.is_human() {
        return forbidden_client();
    }
    match guard.sessions.outbox().list_passkeys(1) {
        Ok(rows) => Json(rows).into_response(),
        Err(_) => internal_error("could not list passkeys"),
    }
}

#[derive(Deserialize)]
pub struct PasskeyRename {
    pub name: String,
}

pub async fn patch_passkey(
    State(guard): State<Arc<ApiGuard>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Extension(principal): axum::Extension<Principal>,
    Json(body): Json<PasskeyRename>,
) -> Response {
    if !principal.is_human() {
        return forbidden_client();
    }
    match guard.sessions.outbox().rename_passkey(1, id, &body.name) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

pub async fn delete_passkey(
    State(guard): State<Arc<ApiGuard>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Extension(principal): axum::Extension<Principal>,
) -> Response {
    if !principal.is_human() {
        return forbidden_client();
    }
    match guard.sessions.outbox().delete_passkey(1, id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => internal_error("could not delete passkey"),
    }
}

#[derive(Serialize)]
pub struct SessionListItem {
    pub id: String,
    pub method: String,
    pub created_at: String,
    pub last_seen_at: String,
    pub user_agent: Option<String>,
    pub current: bool,
}

pub async fn list_auth_sessions(
    State(guard): State<Arc<ApiGuard>>,
    headers: HeaderMap,
    axum::Extension(principal): axum::Extension<Principal>,
) -> Response {
    if !principal.is_human() {
        return forbidden_client();
    }
    let current = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| cookie_value(raw, SESSION_COOKIE))
        .and_then(|id| guard.sessions.touch(id))
        .map(|l| l.public_id);
    let rows = match guard.sessions.outbox().list_auth_sessions(1) {
        Ok(r) => r,
        Err(_) => return internal_error("could not list sessions"),
    };
    let items: Vec<SessionListItem> = rows
        .into_iter()
        .map(|r| SessionListItem {
            id: r.public_id.clone(),
            method: r.method,
            created_at: r.created_at,
            last_seen_at: r.last_seen_at,
            user_agent: r.user_agent,
            current: current.as_ref() == Some(&r.public_id),
        })
        .collect();
    Json(items).into_response()
}

pub async fn delete_auth_session(
    State(guard): State<Arc<ApiGuard>>,
    axum::extract::Path(public_id): axum::extract::Path<String>,
    axum::Extension(principal): axum::Extension<Principal>,
) -> Response {
    if !principal.is_human() {
        return forbidden_client();
    }
    match guard
        .sessions
        .outbox()
        .revoke_auth_session_public(1, &public_id)
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => internal_error("could not revoke session"),
    }
}

fn session_body(
    guard: &ApiGuard,
    csrf: Option<String>,
    method: Option<String>,
    authenticated: bool,
) -> SessionResponse {
    let passkeys_enabled = guard.webauthn.is_some();
    let passkey_count = guard.sessions.outbox().passkey_count(1).unwrap_or(0);
    SessionResponse {
        authenticated,
        login_required: !guard.auth.is_open(),
        csrf,
        method,
        passkeys_enabled,
        passkey_count,
        can_register: passkeys_enabled,
    }
}

fn forbidden_client() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ApiError {
            error: "this route needs the owner session".to_string(),
        }),
    )
        .into_response()
}

fn rate_limited(guard: &ApiGuard) -> Response {
    let secs = guard.rate_limiter.retry_after_secs();
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, secs.to_string())],
        Json(ApiError {
            error: "too many sign-in attempts; try again later".to_string(),
        }),
    )
        .into_response()
}

fn internal_error(msg: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: msg.to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
