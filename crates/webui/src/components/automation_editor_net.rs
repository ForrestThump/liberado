//! HTTP for the Automation Editor dock.
//!
//! Open is one singleton post. A turn is `POST /api/chat` with that session
//! id. The transcript and the pending-card list are read back so Approve's
//! reply, including a read-only refusal, shows in the thread.

use chat_client_contract::{ConversationHistoryResponse, PendingApprovalListResponse};

use super::super::chat::ChatMsg;
use super::logic::{
    editor_chat_body, editor_open_accepted, editor_open_body, editor_transcript, error_from_body,
    pending_ids_for_session, reply_from_chat, session_id_from_open,
};

pub(super) struct EditorSnapshot {
    pub(super) messages: Vec<ChatMsg>,
    pub(super) pending_ids: Vec<String>,
}

pub(super) async fn open_editor(api_base: &str) -> Result<String, String> {
    let url = format!("{api_base}/api/conversations");
    let response = reqwest::Client::new()
        .post(url)
        .json(&editor_open_body())
        .send()
        .await
        .map_err(|error| format!("Failed to reach daemon: {error}"))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Bad response: {error}"))?;
    if !editor_open_accepted(status) {
        return Err(error_from_body(&text, status));
    }
    let body = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    session_id_from_open(&body).ok_or_else(|| "Editor session id missing".to_string())
}

pub(super) async fn post_turn(
    api_base: &str,
    session_id: &str,
    message: &str,
) -> Result<String, String> {
    let url = format!("{api_base}/api/chat");
    let response = reqwest::Client::new()
        .post(url)
        .json(&editor_chat_body(session_id, message))
        .send()
        .await
        .map_err(|error| format!("Failed to reach daemon: {error}"))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Bad response: {error}"))?;
    if !(200..300).contains(&status) {
        return Err(error_from_body(&text, status));
    }
    let body = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    reply_from_chat(&body).ok_or_else(|| "Chat reply missing".to_string())
}

pub(super) async fn load_snapshot(
    api_base: &str,
    session_id: &str,
) -> Result<EditorSnapshot, String> {
    let messages = load_messages(api_base, session_id).await?;
    let pending_ids = load_pending_ids(api_base, session_id)
        .await
        .unwrap_or_default();
    Ok(EditorSnapshot {
        messages,
        pending_ids,
    })
}

async fn load_messages(api_base: &str, session_id: &str) -> Result<Vec<ChatMsg>, String> {
    let url = format!("{api_base}/api/conversations/{session_id}");
    let response = reqwest::get(url)
        .await
        .map_err(|error| format!("Failed to reach daemon: {error}"))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Bad response: {error}"))?;
    if !(200..300).contains(&status) {
        return Err(error_from_body(&text, status));
    }
    let history: ConversationHistoryResponse =
        serde_json::from_str(&text).map_err(|error| format!("Bad response: {error}"))?;
    Ok(editor_transcript(&history.messages))
}

async fn load_pending_ids(api_base: &str, session_id: &str) -> Result<Vec<String>, String> {
    let url = format!("{api_base}/api/approvals/pending");
    let response = reqwest::get(url)
        .await
        .map_err(|error| format!("Failed to reach daemon: {error}"))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Bad response: {error}"))?;
    if !(200..300).contains(&status) {
        return Err(error_from_body(&text, status));
    }
    let body: PendingApprovalListResponse =
        serde_json::from_str(&text).map_err(|error| format!("Bad response: {error}"))?;
    Ok(pending_ids_for_session(&body.approvals, session_id))
}
