//! WebAuthn ceremony HTTP handlers.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chat_client_contract::ApiError;
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use webauthn_rs::prelude::{Passkey, PublicKeyCredential, RegisterPublicKeyCredential, Uuid};

use crate::auth::cookies::{append_cookie, clear_cookie, session_max_age, set_session_cookies};
use crate::auth::challenges::{CeremonyKind, ChallengeStore};
use crate::auth::client_ip;
use crate::auth::guard::{self, can_register_passkeys, ApiGuard};
use crate::auth::sessions::{CEREMONY_COOKIE, SESSION_COOKIE, cookie_value};
use crate::auth::ua_label::passkey_name_from_user_agent;
use crate::auth::webauthn_config;

#[derive(Deserialize)]
pub struct PasskeyFinishBody {
    pub credential: serde_json::Value,
}

pub async fn passkey_login_start(
    State(guard): State<Arc<ApiGuard>>,
    headers: HeaderMap,
) -> Response {
    let Some(webauthn) = guard.webauthn.as_ref() else {
        return not_enabled();
    };
    if !origin_ok(&guard, &headers) {
        return bad_origin();
    }
    let passkeys = match load_passkeys(&guard) {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: "no passkeys are registered yet".to_string(),
                }),
            )
                .into_response();
        }
        Err(resp) => return resp,
    };
    let (rcr, state) = match webauthn.webauthn.start_passkey_authentication(&passkeys) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "passkey login start failed");
            return internal_error("could not start passkey sign-in");
        }
    };
    respond_with_options(&guard.challenges, CeremonyKind::Login(state), rcr)
}

pub async fn passkey_login_finish(
    State(guard): State<Arc<ApiGuard>>,
    headers: HeaderMap,
    Json(body): Json<PasskeyFinishBody>,
) -> Response {
    let ip = client_ip::client_ip(&headers, None);
    let Some(webauthn) = guard.webauthn.as_ref() else {
        return not_enabled();
    };
    let ceremony_id = match ceremony_from_cookie(&headers) {
        Some(id) => id,
        None => return invalid_ceremony(),
    };
    let auth_state = match guard.challenges.take_login(&ceremony_id) {
        Some(s) => s,
        None => return invalid_ceremony(),
    };
    let reg: PublicKeyCredential = match serde_json::from_value(body.credential) {
        Ok(v) => v,
        Err(_) => {
            if guard.rate_limiter.record_failure(&ip) {
                return rate_limited(&guard);
            }
            return bad_request("malformed passkey response");
        }
    };
    let result = match webauthn.webauthn.finish_passkey_authentication(&reg, &auth_state) {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(?err, client = %ip, "passkey login finish failed");
            if guard.rate_limiter.record_failure(&ip) {
                return rate_limited(&guard);
            }
            return (
                StatusCode::UNAUTHORIZED,
                Json(ApiError {
                    error: "passkey sign-in failed".to_string(),
                }),
            )
                .into_response();
        }
    };
    let cred_id = result.cred_id();
    let row = match guard
        .sessions
        .outbox()
        .passkey_by_credential_id(cred_id.as_ref())
    {
        Ok(Some(r)) => r,
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(ApiError {
                    error: "unknown passkey".to_string(),
                }),
            )
                .into_response();
        }
    };
    let mut passkey: Passkey = match serde_json::from_str(&row.passkey_json) {
        Ok(p) => p,
        Err(_) => return internal_error("passkey store corrupt"),
    };
    let _ = passkey.update_credential(&result);
    let json = serde_json::to_string(&passkey).unwrap_or_default();
    let _ = guard.sessions.outbox().update_passkey_after_auth(
        row.id,
        &json,
        result.counter(),
        Utc::now(),
    );
    mint_full_session(&guard, &headers, "passkey")
}

pub async fn passkey_register_start(
    State(guard): State<Arc<ApiGuard>>,
    headers: HeaderMap,
) -> Response {
    if !can_register_passkeys(&guard, &headers) {
        if session_is_passkey(&guard, &headers) {
            return (
                StatusCode::FORBIDDEN,
                Json(ApiError {
                    error: guard::passkey_session_forbidden_message().to_string(),
                }),
            )
                .into_response();
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "passkey registration requires the emergency token or a token-method session"
                    .to_string(),
            }),
        )
            .into_response();
    }
    let Some(webauthn) = guard.webauthn.as_ref() else {
        return not_enabled();
    };
    if !origin_ok(&guard, &headers) {
        return bad_origin();
    }
    let user_bytes = guard.sessions.outbox().owner_webauthn_user_id().unwrap_or_default();
    let user_uuid = uuid_from_bytes(&user_bytes);
    let exclude = existing_credential_ids(&guard);
    let (ccr, state) = match webauthn.webauthn.start_passkey_registration(
        user_uuid,
        "owner",
        "Owner",
        exclude,
    ) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "passkey register start failed");
            return internal_error("could not start passkey registration");
        }
    };
    respond_with_options(&guard.challenges, CeremonyKind::Register(state), ccr)
}

pub async fn passkey_register_finish(
    State(guard): State<Arc<ApiGuard>>,
    headers: HeaderMap,
    Json(body): Json<PasskeyFinishBody>,
) -> Response {
    let ip = client_ip::client_ip(&headers, None);
    if !can_register_passkeys(&guard, &headers) {
        if session_is_passkey(&guard, &headers) {
            return (
                StatusCode::FORBIDDEN,
                Json(ApiError {
                    error: guard::passkey_session_forbidden_message().to_string(),
                }),
            )
                .into_response();
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "passkey registration requires the emergency token or a token-method session"
                    .to_string(),
            }),
        )
            .into_response();
    }
    let Some(webauthn) = guard.webauthn.as_ref() else {
        return not_enabled();
    };
    let ceremony_id = match ceremony_from_cookie(&headers) {
        Some(id) => id,
        None => return invalid_ceremony(),
    };
    let reg_state = match guard.challenges.take_register(&ceremony_id) {
        Some(s) => s,
        None => return invalid_ceremony(),
    };
    let reg: RegisterPublicKeyCredential = match serde_json::from_value(body.credential) {
        Ok(v) => v,
        Err(_) => {
            if guard.rate_limiter.record_failure(&ip) {
                return rate_limited(&guard);
            }
            return bad_request("malformed passkey response");
        }
    };
    let passkey = match webauthn.webauthn.finish_passkey_registration(&reg, &reg_state) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(?err, client = %ip, "passkey register finish failed");
            if guard.rate_limiter.record_failure(&ip) {
                return rate_limited(&guard);
            }
            return bad_request("passkey registration failed");
        }
    };
    let name = passkey_name_from_user_agent(user_agent(&headers).unwrap_or_default());
    let json = serde_json::to_string(&passkey).unwrap_or_default();
    let _ = guard.sessions.outbox().insert_passkey(
        1,
        passkey.cred_id().as_ref(),
        &json,
        &name,
        Utc::now(),
    );
    mint_full_session(&guard, &headers, "passkey")
}

fn load_passkeys(guard: &ApiGuard) -> Result<Vec<Passkey>, Response> {
    let jsons = guard
        .sessions
        .outbox()
        .passkey_json_values(1)
        .map_err(|_| internal_error("passkey store error"))?;
    let mut out = Vec::with_capacity(jsons.len());
    for json in jsons {
        let pk: Passkey =
            serde_json::from_str(&json).map_err(|_| internal_error("passkey store corrupt"))?;
        out.push(pk);
    }
    Ok(out)
}

fn existing_credential_ids(
    guard: &ApiGuard,
) -> Option<Vec<webauthn_rs::prelude::CredentialID>> {
    let passkeys = load_passkeys(guard).ok()?;
    Some(passkeys.iter().map(|p| p.cred_id().clone()).collect())
}

fn uuid_from_bytes(bytes: &[u8]) -> Uuid {
    let mut id = [0u8; 16];
    let len = bytes.len().min(16);
    id[..len].copy_from_slice(&bytes[..len]);
    Uuid::from_bytes(id)
}

fn respond_with_options<T: Serialize>(
    store: &ChallengeStore,
    kind: CeremonyKind,
    challenge: T,
) -> Response {
    let mut ceremony = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut ceremony);
    let ceremony_id = ceremony.iter().map(|b| format!("{b:02x}")).collect::<String>();
    store.insert(ceremony_id.clone(), kind);
    let mut headers = HeaderMap::new();
    append_cookie(
        &mut headers,
        &format!(
            "{CEREMONY_COOKIE}={ceremony_id}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=300"
        ),
    );
    (
        headers,
        Json(serde_json::json!({ "options": challenge })),
    )
        .into_response()
}

fn mint_full_session(guard: &ApiGuard, headers: &HeaderMap, method: &str) -> Response {
    let ua = user_agent(headers);
    let (session_id, csrf) = match guard.sessions.create(1, method, ua) {
        Ok(v) => v,
        Err(_) => return internal_error("could not create session"),
    };
    let live = guard.sessions.touch(&session_id).expect("just created");
    let max_age = session_max_age(&live.expires_at);
    let mut out = HeaderMap::new();
    set_session_cookies(&mut out, &session_id, &csrf, max_age);
    clear_cookie(&mut out, CEREMONY_COOKIE);
    (
        out,
        Json(serde_json::json!({
            "authenticated": true,
            "login_required": true,
            "csrf": csrf,
            "method": method,
        })),
    )
        .into_response()
}

fn ceremony_from_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| cookie_value(raw, CEREMONY_COOKIE))
        .map(str::to_owned)
}

fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
}

fn session_is_passkey(guard: &ApiGuard, headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| cookie_value(raw, SESSION_COOKIE))
        .and_then(|id| guard.sessions.touch(id))
        .is_some_and(|s| s.method == "passkey")
}

fn origin_ok(guard: &ApiGuard, headers: &HeaderMap) -> bool {
    let Some(webauthn) = guard.webauthn.as_ref() else {
        return true;
    };
    if let Some(origin) = headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok()) {
        return webauthn_config::origin_allowed(origin, &webauthn.origins);
    }
    if let Some(host) = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        return webauthn_config::origin_allowed(host, &webauthn.origins)
            || webauthn_config::is_loopback_host(host);
    }
    false
}

fn rate_limited(guard: &ApiGuard) -> Response {
    let secs = guard.rate_limiter.retry_after_secs();
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(axum::http::header::RETRY_AFTER, secs.to_string())],
        Json(ApiError {
            error: "too many sign-in attempts; try again later".to_string(),
        }),
    )
        .into_response()
}

fn not_enabled() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: "passkeys are not enabled on this server".to_string(),
        }),
    )
        .into_response()
}

fn bad_origin() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ApiError {
            error: "origin is not allowed for passkeys".to_string(),
        }),
    )
        .into_response()
}

fn invalid_ceremony() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: "passkey ceremony expired or already used".to_string(),
        }),
    )
        .into_response()
}

fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: msg.to_string(),
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
