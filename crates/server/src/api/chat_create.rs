//! `POST /api/conversations` — create-path for a chat, a profiled chat, or the Agent Creator.

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
///
/// `agent_creator: true` ignores `profile` and `title` and find-or-creates the singleton Agent
/// Creator session (201 the first time, 200 after that). See
/// `docs/spec/architecture/chat-agent-surface-mode.md` §6e.
#[derive(Deserialize, Default)]
pub struct CreateConversationRequest {
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// Agents shelf +. Find-or-create the flagged creator session. Default false, so `{}` and
    /// `{profile}` keep their existing meaning.
    #[serde(default)]
    pub agent_creator: bool,
}

/// `POST /api/conversations` — create a conversation and return its header.
///
/// Agents shelf + sends `agent_creator: true`. A profile body is the older create-with-grant
/// path (still used by non-WebUI clients). `{}` is a default-grant Chat. Does not change New
/// Chat (nonce / first-message default grant).
pub async fn create_conversation(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateConversationRequest>,
) -> impl IntoResponse {
    if req.agent_creator {
        return open_agent_creator_response(state).await;
    }
    create_standard_conversation(state, req).await
}

async fn open_agent_creator_response(state: Arc<AppState>) -> axum::response::Response {
    let Some(sessions) = &state.chat else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: CHAT_DISABLED_HINT.into(),
            }),
        )
            .into_response();
    };
    let (id, created) = match sessions.open_agent_creator().await {
        Ok(pair) => pair,
        Err(msg) => {
            return (StatusCode::BAD_REQUEST, Json(ApiError { error: msg })).into_response();
        }
    };
    match sessions.list().await {
        Ok(headers) => header_or_missing(headers, id, created),
        Err(e) => chat_error(e),
    }
}

fn header_or_missing(
    headers: Vec<liberado_conversation_store::ConversationHeader>,
    id: liberado_conversation_store::Ulid,
    created: bool,
) -> axum::response::Response {
    if let Some(header) = headers.into_iter().find(|h| h.id == id) {
        let status = if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        (status, Json(header)).into_response()
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

async fn create_standard_conversation(
    state: Arc<AppState>,
    req: CreateConversationRequest,
) -> axum::response::Response {
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
        Ok(headers) => header_or_missing(headers, id, true),
        Err(e) => chat_error(e),
    }
}
