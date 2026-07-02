//! Typed HTTP client for the Liberado server API.
//!
//! Every endpoint the TUI calls lives here, each returning a typed struct so the rest
//! of the crate never touches raw JSON. The client is a thin `reqwest` wrapper — no
//! caching, no retry (the poller in `main.rs` handles timing).
//!
//! Wire DTOs (`DaemonStatus`, `ReactionEvent`, `ConvHeader`, `ChatMessage`) are imported
//! from `chat-client-contract` — the single source of truth. The display-only chips
//! (`ToolCallChip`, `ToolResultChip`) remain here because they are constructed from
//! `ChatEvent` data and never serialized.

use reqwest::{Client, StatusCode};

// Re-export the shared wire types so the rest of the crate can still import them
// from `crate::api::*` without changing call sites.
pub use chat_client_contract::{ChatMessage, ConvHeader, DaemonStatus, ReactionEvent};

/// Wrapper for `GET /api/conversations/{id}` response: `{"messages": […]}`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ConversationHistory {
    messages: Vec<ChatMessage>,
}

/// A tool-call chip rendered inline in the chat: `[tool] name(args preview)`.
/// Display-only — constructed from `ChatEvent::Tool` data, never serialized to JSON.
#[derive(Debug, Clone)]
pub struct ToolCallChip {
    pub name: String,
    pub args: String,
}

/// The outcome chip for a completed tool call: `[tool] name ok|err preview`.
/// Display-only — constructed from `ChatEvent::ToolResult` data, never serialized.
#[derive(Debug, Clone)]
pub struct ToolResultChip {
    pub name: String,
    pub ok: bool,
    pub preview: String,
}

/// Fetch `GET /api/status` and return the parsed `DaemonStatus`.
pub async fn fetch_status(
    client: &Client,
    server: &str,
) -> Result<Option<DaemonStatus>, reqwest::Error> {
    let resp = client.get(format!("{server}/api/status")).send().await?;
    if resp.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Ok(None);
    }
    resp.error_for_status_ref()?;
    Ok(Some(resp.json().await?))
}

/// Fetch `GET /api/reactions?limit=N` and return the tail entries.
pub async fn fetch_reactions(
    client: &Client,
    server: &str,
    limit: usize,
) -> Result<Vec<ReactionEvent>, reqwest::Error> {
    let resp = client
        .get(format!("{server}/api/reactions"))
        .query(&[("limit", limit)])
        .send()
        .await?;
    if resp.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Ok(Vec::new());
    }
    resp.error_for_status_ref()?;
    resp.json().await
}

/// Fetch `GET /api/conversations` and return the header list.
pub async fn fetch_conversations(
    client: &Client,
    server: &str,
) -> Result<Vec<ConvHeader>, reqwest::Error> {
    let resp = client
        .get(format!("{server}/api/conversations"))
        .send()
        .await?;
    if resp.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Ok(Vec::new());
    }
    resp.error_for_status_ref()?;
    resp.json().await
}

/// Fetch `GET /api/conversations/{id}` and return the message history.
pub async fn fetch_conversation_history(
    client: &Client,
    server: &str,
    id: &str,
) -> Result<Option<Vec<ChatMessage>>, reqwest::Error> {
    let resp = client
        .get(format!("{server}/api/conversations/{id}"))
        .send()
        .await?;
    if resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Ok(None);
    }
    resp.error_for_status_ref()?;
    let history: ConversationHistory = resp.json().await?;
    Ok(Some(history.messages))
}

/// Open a streaming `POST /api/chat/stream` with `message` and optional `session`, and
/// return a byte stream (`reqwest::Response`) for the SSE decoder to consume.
pub async fn post_chat_stream(
    client: &Client,
    server: &str,
    message: &str,
    session: Option<&str>,
) -> Result<reqwest::Response, reqwest::Error> {
    let body = serde_json::json!({ "message": message, "session": session });
    client
        .post(format!("{server}/api/chat/stream"))
        .json(&body)
        .send()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_client_contract::ReactionOutcome;

    #[test]
    fn daemon_status_serde_roundtrip() {
        let status = DaemonStatus {
            running: true,
            vault_path: "/home/user/vault".into(),
            uptime_seconds: 3600,
            watcher_active: false,
            dispatcher_attached: true,
            orchestrator_attached: true,
            reactions_seen: 42,
            model_name: Some("deepseek-chat".into()),
            token_usage_total: Some(1500),
            context_window: Some(128000),
            chat_tools: 1,
            chat_tool_names: vec!["tasks:add".into()],
        };
        let json = serde_json::to_value(&status).unwrap();
        let back: DaemonStatus = serde_json::from_value(json).unwrap();
        assert!(back.running);
        assert_eq!(back.vault_path, "/home/user/vault");
        assert_eq!(back.uptime_seconds, 3600);
        assert!(back.dispatcher_attached);
        assert_eq!(back.reactions_seen, 42);
        assert_eq!(back.model_name, Some("deepseek-chat".into()));
        assert_eq!(back.token_usage_total, Some(1500));
        assert_eq!(back.context_window, Some(128000));
        assert_eq!(back.chat_tools, 1);
    }

    #[test]
    fn daemon_status_missing_model_fields_defaults_to_none() {
        let json = serde_json::json!({
            "running": true,
            "vault_path": "/v",
            "uptime_seconds": 100,
            "watcher_active": false,
            "dispatcher_attached": false,
            "orchestrator_attached": false,
            "reactions_seen": 0
        });
        let status: DaemonStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status.model_name, None);
        assert_eq!(status.token_usage_total, None);
        assert_eq!(status.context_window, None);
    }

    #[test]
    fn reaction_event_serde_roundtrip() {
        let event = ReactionEvent {
            event_type: "file_changed".into(),
            timestamp: "2025-06-25T12:00:00Z".into(),
            source: "watcher".into(),
            correlation_id: "abc-123".into(),
            path: Some("/docs/notes.md".into()),
            outcome: ReactionOutcome::Observed,
        };
        let json = serde_json::to_value(&event).unwrap();
        let back: ReactionEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back.event_type, "file_changed");
        assert_eq!(back.path, Some("/docs/notes.md".into()));
        assert_eq!(back.outcome, ReactionOutcome::Observed);
    }

    #[test]
    fn reaction_event_null_path_deserializes() {
        let json = serde_json::json!({
            "event_type": "noop",
            "timestamp": "2025-06-25T12:00:00Z",
            "source": "watcher",
            "correlation_id": "x",
            "path": null,
            "outcome": "observed"
        });
        let event: ReactionEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event.path, None);
    }

    #[test]
    fn conv_header_serde_roundtrip_with_parent() {
        let header = ConvHeader {
            id: "c1".into(),
            title: Some("test conversation".into()),
            created_at: "2025-06-25T12:00:00Z".into(),
            parent_conversation: Some("c0".into()),
            spawned_by: Some("msg-5".into()),
        };
        let json = serde_json::to_value(&header).unwrap();
        let back: ConvHeader = serde_json::from_value(json).unwrap();
        assert_eq!(back.id, "c1");
        assert_eq!(back.parent_conversation, Some("c0".into()));
        assert_eq!(back.spawned_by, Some("msg-5".into()));
    }

    #[test]
    fn conv_header_missing_parent_fields_defaults_to_none() {
        let json = serde_json::json!({
            "id": "c2",
            "title": "plain",
            "created_at": "2025-06-25T12:00:00Z"
        });
        let header: ConvHeader = serde_json::from_value(json).unwrap();
        assert_eq!(header.id, "c2");
        assert_eq!(header.parent_conversation, None);
        assert_eq!(header.spawned_by, None);
    }

    #[test]
    fn conv_header_default_is_all_empty() {
        let header = ConvHeader::default();
        assert!(header.id.is_empty());
        assert!(header.title.is_none());
        assert!(header.parent_conversation.is_none());
    }

    #[test]
    fn chat_message_serde_roundtrip_with_tool_calls() {
        let msg = ChatMessage {
            role: "assistant".into(),
            content: "Let me search...".into(),
            tool_calls: Some(serde_json::json!([
                {"function": {"name": "search", "arguments": "{\"q\":\"test\"}"}}
            ])),
            tool_call_id: None,
        };
        let json = serde_json::to_value(&msg).unwrap();
        let back: ChatMessage = serde_json::from_value(json).unwrap();
        assert_eq!(back.role, "assistant");
        assert_eq!(back.content, "Let me search...");
        let tc = back.tool_calls.unwrap();
        assert_eq!(tc[0]["function"]["name"], "search");
    }

    #[test]
    fn chat_message_missing_tool_fields_defaults() {
        let json = serde_json::json!({"role": "user", "content": "hello"});
        let msg: ChatMessage = serde_json::from_value(json).unwrap();
        assert_eq!(msg.role, "user");
        assert_eq!(msg.tool_calls, None);
        assert_eq!(msg.tool_call_id, None);
    }

    #[test]
    fn conversation_history_deserialization() {
        let json = serde_json::json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"}
            ]
        });
        let history: super::ConversationHistory = serde_json::from_value(json).unwrap();
        assert_eq!(history.messages.len(), 2);
        assert_eq!(history.messages[0].role, "user");
    }

    #[test]
    fn tool_call_result_chip_construction() {
        let call = ToolCallChip {
            name: "search".into(),
            args: "{\"q\":\"test\"}".into(),
        };
        assert_eq!(call.name, "search");
        let result = ToolResultChip {
            name: "search".into(),
            ok: true,
            preview: "3 results".into(),
        };
        assert!(result.ok);
        assert_eq!(result.preview, "3 results");
    }
}
