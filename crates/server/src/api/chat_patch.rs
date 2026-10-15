//! `PATCH /api/conversations/{id}` — title, project, and the next turn's model.

use std::sync::Arc;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use liberado_conversation_store::{ChatMetaUpdate, Ulid};
use serde::Deserialize;

use chat_client_contract::ApiError;

use super::CHAT_DISABLED_HINT;
use super::chat_error;
use crate::state::AppState;

/// Body of `PATCH /api/conversations/{id}`.
///
/// A missing field is left unchanged. `null` clears `project` or `pending_model`.
/// `title` has no clear: omit it to leave the title alone.
#[derive(Debug, Deserialize)]
pub struct PatchConversation {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "tri_state")]
    pub project: Option<Option<String>>,
    #[serde(default, deserialize_with = "tri_state")]
    pub pending_model: Option<Option<String>>,
}

fn tri_state<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(deserializer)?))
}

pub async fn apply(state: Arc<AppState>, id: Ulid, req: PatchConversation) -> Response {
    let Some(sessions) = &state.chat else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: CHAT_DISABLED_HINT.into(),
            }),
        )
            .into_response();
    };
    if req.title.is_none() && req.project.is_none() && req.pending_model.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "nothing to update".into(),
            }),
        )
            .into_response();
    }
    let project = match checked_project(&state, req.project) {
        Ok(project) => project,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(ApiError { error })).into_response();
        }
    };
    let update = ChatMetaUpdate {
        project,
        pending_model: checked_model(req.pending_model),
    };
    if let Err(error) = sessions.set_chat_meta(id, update).await {
        return store_error(id, error);
    }
    if let Some(title) = req.title
        && let Err(error) = sessions.set_title(id, title).await
    {
        return store_error(id, error);
    }
    StatusCode::OK.into_response()
}

/// `None` leaves the project. `Some(None)` clears it. A name must be an enabled project.
fn checked_project(
    state: &AppState,
    project: Option<Option<String>>,
) -> Result<Option<Option<String>>, String> {
    let Some(project) = project else {
        return Ok(None);
    };
    let Some(name) = project else {
        return Ok(Some(None));
    };
    let name = name.trim();
    if name.is_empty() {
        return Ok(Some(None));
    }
    if state.config.project_by_name(name).is_none() {
        return Err(format!("unknown project: {name}"));
    }
    Ok(Some(Some(name.to_string())))
}

/// `None` leaves the pick. Blank text clears it. Any other string is stored unchanged.
fn checked_model(model: Option<Option<String>>) -> Option<Option<String>> {
    let model = model?;
    let Some(model) = model else {
        return Some(None);
    };
    let model = model.trim();
    if model.is_empty() {
        Some(None)
    } else {
        Some(Some(model.to_string()))
    }
}

fn store_error(id: Ulid, error: liberado_main_agent::SessionError) -> Response {
    match error {
        liberado_main_agent::SessionError::Store(
            liberado_conversation_store::StoreError::NotFound(_),
        ) => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "conversation not found".into(),
            }),
        )
            .into_response(),
        other => {
            tracing::warn!(error = %other, "PATCH failed for conversation {id}");
            chat_error(other)
        }
    }
}

#[cfg(test)]
#[path = "chat_patch_http_tests.rs"]
mod http_tests;
