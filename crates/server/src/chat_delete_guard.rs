//! Refuse deletion of the built-in Automation Editor.
//!
//! The sidebar button is owned by the WebUI. This guard is the server answer:
//! `DELETE` returns 409 and the row stays.

use axum::Json;
use axum::http::StatusCode;
use chat_client_contract::ApiError;
use liberado_conversation_store::Ulid;
use liberado_session_store::SessionStore;

/// `Some` when `id` is the Automation Editor singleton.
pub async fn refuse_automation_editor(
    sessions: &SessionStore,
    id: Ulid,
) -> Option<(StatusCode, Json<ApiError>)> {
    let header = sessions.session(id).await?;
    if !header.automation_editor {
        return None;
    }
    Some((
        StatusCode::CONFLICT,
        Json(ApiError {
            error: "The built-in Automation Editor cannot be deleted.".into(),
        }),
    ))
}
