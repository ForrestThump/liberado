//! Typed HTTP client for the Liberado server API.
//!
//! Every endpoint the TUI calls lives here, each returning a typed struct so the rest
//! of the crate never touches raw JSON. The client is a thin `reqwest` wrapper — no
//! caching, no retry (the poller in `main.rs` handles timing).

use reqwest::Client;

/// Response shape from `GET /api/status`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub vault_path: String,
    pub uptime_seconds: u64,
    pub watcher_active: bool,
    pub dispatcher_attached: bool,
    pub orchestrator_attached: bool,
    pub reactions_seen: u64,
}

/// One entry from `GET /api/reactions`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReactionEvent {
    pub event_type: String,
    pub timestamp: String,
    pub source: String,
    pub correlation_id: String,
    pub path: Option<String>,
    pub outcome: String,
}

/// One conversation header from `GET /api/conversations`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConvHeader {
    pub id: String,
    pub title: String,
    pub created_at: String,
}

/// A message from `GET /api/conversations/{id}` history.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub tool_calls: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// A tool-call chip rendered inline in the chat: `[tool] name(args preview)`.
#[derive(Debug, Clone)]
pub struct ToolCallChip {
    pub name: String,
    pub args: String,
}

/// The outcome chip for a completed tool call: `[tool] name ok|err preview`.
#[derive(Debug, Clone)]
pub struct ToolResultChip {
    pub name: String,
    pub ok: bool,
    pub preview: String,
}

/// Fetch `GET /api/status` and return the parsed `DaemonStatus`.
pub async fn fetch_status(client: &Client, server: &str) -> Result<DaemonStatus, reqwest::Error> {
    todo!("fetch_status")
}

/// Fetch `GET /api/reactions?limit=N` and return the tail entries.
pub async fn fetch_reactions(
    client: &Client,
    server: &str,
    limit: usize,
) -> Result<Vec<ReactionEvent>, reqwest::Error> {
    todo!("fetch_reactions")
}

/// Fetch `GET /api/conversations` and return the header list.
pub async fn fetch_conversations(
    client: &Client,
    server: &str,
) -> Result<Vec<ConvHeader>, reqwest::Error> {
    todo!("fetch_conversations")
}

/// Fetch `GET /api/conversations/{id}` and return the message history.
pub async fn fetch_conversation_history(
    client: &Client,
    server: &str,
    id: &str,
) -> Result<Vec<ChatMessage>, reqwest::Error> {
    todo!("fetch_conversation_history")
}

/// Open a streaming `POST /api/chat/stream` with `message` and optional `session`, and
/// return a byte stream (`reqwest::Response`) for the SSE decoder to consume.
pub async fn post_chat_stream(
    client: &Client,
    server: &str,
    message: &str,
    session: Option<&str>,
) -> Result<reqwest::Response, reqwest::Error> {
    todo!("post_chat_stream")
}
