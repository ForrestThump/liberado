//! `POST /api/conversations` — create-path for New Agent / explicit create-with-profile.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;

use chat_client_contract::ApiError;

use crate::state::AppState;

use super::chat::{CHAT_DISABLED_HINT, chat_error, resolve_chat_grant};

/// Body of `POST /api/conversations` — open a conversation without sending a first message.
///
/// Optional `profile` resolves via `Config::resolve_session_profile` (fail closed) and creates
/// through `create_with_grant`, so an agent-eligible name stamps `surface_mode: agent` (Reading B).
/// Absent profile → default-grant Chat (same as New Chat's eventual first-message path).
#[derive(Deserialize, Default)]
pub struct CreateConversationRequest {
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST /api/conversations` — create a conversation (optional profile) and return its header.
///
/// Used by WebUI **New Agent** so the row appears on the Agents shelf before the first message.
/// Does not change New Chat (nonce / first-message default grant).
pub async fn create_conversation(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateConversationRequest>,
) -> impl IntoResponse {
    let Some(sessions) = &state.chat else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: CHAT_DISABLED_HINT.into(),
            }),
        )
            .into_response();
    };

    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    let id = match req
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => match sessions.create(title).await {
            Ok(id) => id,
            Err(e) => return chat_error(e),
        },
        Some(name) => {
            // `resolve_chat_grant` is `fn(Option<&str>) -> Result<Option<Grant>, String>` because
            // the absent-profile arm returns `Ok(None)` to the caller (no resolve needed).
            // When WE pass `Some(name)` here, the inner `Some(name)` arm can only produce
            // `Ok(Some(grant))` (profile found, overrides serialized) or `Err(msg)` (unknown /
            // disabled / serialization failure — all fail closed). `Ok(None)` is structurally
            // unreachable for this input. If you ever generalize `resolve_chat_grant` to return
            // `Ok(None)` for any `Some(name)` case, this site is the one that must change.
            let grant = match resolve_chat_grant(state.config.as_ref(), Some(name)) {
                Ok(Some(grant)) => grant,
                Ok(None) => unreachable!(
                    "resolve_chat_grant(Some(name)) cannot return Ok(None) — only Ok(Some(grant)) \
                     or Err; the Ok(None) arm is reserved for the absent-profile caller"
                ),
                Err(msg) => {
                    return (StatusCode::BAD_REQUEST, Json(ApiError { error: msg }))
                        .into_response();
                }
            };
            match sessions.create_with_grant(title, grant).await {
                Ok(id) => id,
                Err(e) => return chat_error(e),
            }
        }
    };

    match sessions.list().await {
        Ok(headers) => {
            if let Some(header) = headers.into_iter().find(|h| h.id == id) {
                (StatusCode::CREATED, Json(header)).into_response()
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError {
                        error: format!("created {id} but header missing from list"),
                    }),
                )
                    .into_response()
            }
        }
        Err(e) => chat_error(e),
    }
}
