//! Wire DTOs — wasm-clean, serde only. No native deps (no tokio, futures, async-trait, ulid).
//!
//! These types are the single source of truth for every HTTP/SSE response shape shared
//! between the server, TUI, WebUI, and CLI.

use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────
// Chat streaming types
// ──────────────────────────────────────────────────────────────

/// A streaming chat event — maps 1:1 with the SSE events produced by the server's
/// `/api/chat/stream` endpoint.
///
/// **Serialization note:** `ChatEvent` uses `#[serde(tag = "type")]` for JSON round-trips
/// (used in tests and non-SSE contexts). When parsing from a live SSE stream, use
/// [`ChatEvent::from_sse_data`] instead — the SSE wire format carries the event type in
/// the `event:` line, not in the JSON payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    /// The conversation session id — the first event emitted in a stream.
    Session { id: String },
    /// An incremental text token of the assistant's reply.
    Token { text: String },
    /// A tool call is starting.
    Tool {
        name: String,
        args: serde_json::Value,
    },
    /// A tool call completed.
    ToolResult {
        name: String,
        ok: bool,
        preview: String,
    },
    /// The turn completed successfully.
    Done,
    /// Something failed.
    Failed { message: String },
}

impl ChatEvent {
    /// Parse from an SSE data payload where the event type is already known from the SSE
    /// `event:` line (so no `"type"` field exists in the JSON).
    ///
    /// **Unknown event types** return `Ok(ChatEvent::Token { text: String::new() })` — a
    /// benign no-op — so forward-compatible servers can add new event types without
    /// breaking older clients. Only *malformed JSON* for a *known* `tool`/`tool_result`
    /// event returns `Err`.
    pub fn from_sse_data(event_type: &str, data: &str) -> Result<Self, serde_json::Error> {
        match event_type {
            "session" => Ok(ChatEvent::Session {
                id: data.to_string(),
            }),
            "token" => Ok(ChatEvent::Token {
                text: data.to_string(),
            }),
            "tool" => {
                #[derive(Deserialize)]
                struct ToolPayload {
                    name: String,
                    args: serde_json::Value,
                }
                let p: ToolPayload = serde_json::from_str(data)?;
                Ok(ChatEvent::Tool {
                    name: p.name,
                    args: p.args,
                })
            }
            "tool_result" => {
                #[derive(Deserialize)]
                struct ToolResultPayload {
                    name: String,
                    ok: bool,
                    preview: String,
                }
                let p: ToolResultPayload = serde_json::from_str(data)?;
                Ok(ChatEvent::ToolResult {
                    name: p.name,
                    ok: p.ok,
                    preview: p.preview,
                })
            }
            "done" => Ok(ChatEvent::Done),
            "failed" => Ok(ChatEvent::Failed {
                message: data.to_string(),
            }),
            // Unknown event type — benign no-op, forward-compatible.
            _ => Ok(ChatEvent::Token {
                text: String::new(),
            }),
        }
    }
}

/// The error type for chat client operations.
/// `thiserror` is a proc-macro with no runtime deps and compiles to wasm32.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("deserialization: {0}")]
    Deserialize(String),
    #[error("chat disabled: {0}")]
    Disabled(String),
}

/// A non-streaming chat response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub reply: String,
    pub session: String,
}

// ──────────────────────────────────────────────────────────────
// Daemon status
// ──────────────────────────────────────────────────────────────

/// Response shape from `GET /api/status`.
/// All optional fields use `#[serde(default)]` so clients tolerate server versions
/// that don't emit them yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub vault_path: String,
    #[serde(default)]
    pub uptime_seconds: u64,
    pub watcher_active: bool,
    pub dispatcher_attached: bool,
    pub orchestrator_attached: bool,
    pub reactions_seen: u64,
    /// Model name (e.g. "deepseek-chat") — available when server wires it.
    #[serde(default)]
    pub model_name: Option<String>,
    /// Total tokens consumed in the current session (prompt + completion).
    #[serde(default)]
    pub token_usage_total: Option<u64>,
    /// Provider context window size in tokens.
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Number of tools available to the chat agent (0 = conversation-only).
    #[serde(default)]
    pub chat_tools: usize,
    /// Names of the tools the chat agent can call.
    #[serde(default)]
    pub chat_tool_names: Vec<String>,
}

/// Response shape from `GET /api/models` — live catalog from the provider's
/// OpenAI-compatible `GET /models`, plus the model currently configured for chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelsResponse {
    /// Model ids from the provider (may be empty if unsupported or failed).
    #[serde(default)]
    pub models: Vec<String>,
    /// Active model id from daemon config (`Provider::model()`), when a provider is attached.
    #[serde(default)]
    pub current: Option<String>,
    /// Soft error when the provider list could not be fetched (UI still shows `current`).
    #[serde(default)]
    pub error: Option<String>,
}

// ──────────────────────────────────────────────────────────────
// Reactions
// ──────────────────────────────────────────────────────────────

/// One entry from `GET /api/reactions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReactionEvent {
    pub event_type: String,
    /// ISO-8601 timestamp string. Kept as `String` so the wire crate avoids a `chrono`
    /// dependency. Parse for display when needed.
    pub timestamp: String,
    pub source: String,
    pub correlation_id: String,
    pub path: Option<String>,
    pub outcome: ReactionOutcome,
}

/// How far a reaction progressed. The three `Acted` sub-variants from the daemon
/// (`acted:reported`, `acted:clarify`, `acted:proposed`) are intentionally collapsed into
/// a single `Acted` on the wire — if the distinction is needed later, re-add sub-variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionOutcome {
    Observed,
    Decided,
    Acted,
}

// ──────────────────────────────────────────────────────────────
// Conversations
// ──────────────────────────────────────────────────────────────

/// One conversation header from `GET /api/conversations`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConvHeader {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub created_at: String,
    /// Parent conversation id — set when this conversation was forked/spawned from another.
    #[serde(default)]
    pub parent_conversation: Option<String>,
    /// The message node that spawned this conversation, when applicable.
    #[serde(default)]
    pub spawned_by: Option<String>,
}

/// A message from `GET /api/conversations/{id}` history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub tool_calls: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// Response from `GET /api/conversations/{id}`. Not just `Vec<ChatMessage>` directly at the top
/// level so a future field (e.g. the conversation's own title/header) can be added without
/// changing the response from a bare array to an object — a breaking shape change for every
/// client, whereas adding a field to an existing object isn't.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationHistoryResponse {
    pub messages: Vec<ChatMessage>,
}

// ──────────────────────────────────────────────────────────────
// Vault / catalog
// ──────────────────────────────────────────────────────────────

/// Response from `GET /api/vault`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultInfo {
    pub root: String,
    pub note_count: u64,
    pub watcher_active: bool,
}

/// Information about an MCP server returned by `GET /api/catalog`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpInfo {
    pub name: String,
    pub description: String,
    pub consequence: String,
    /// Number of tools this MCP exposes.
    /// `#[serde(default)]` — currently always 0 from the server (no per-MCP tool count
    /// source yet; populated by clients from tool runtime metadata in the future).
    #[serde(default)]
    pub tool_count: usize,
    /// Names of tools this MCP exposes.
    #[serde(default)]
    pub tool_names: Vec<String>,
    /// Provenance / origin tag — identifies how the MCP was registered (e.g. config file path).
    /// `None` when absent in older server responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    /// Whether chat's own tool surface (the `"main-agent"` component in `policy.toml`) is
    /// granted this MCP. `#[serde(default)]` so an older server that doesn't emit this yet just
    /// reads as `false` (a missing badge, not a broken response).
    #[serde(default)]
    pub visible_to_main_agent: bool,
    /// Whether the dispatcher/orchestrator pipeline (the `"dispatcher"` component in
    /// `policy.toml`) is granted this MCP. Independent of `visible_to_main_agent` — an MCP can
    /// be granted to one component, both, or neither (see Decision 4's capability narrowing).
    #[serde(default)]
    pub visible_to_dispatcher: bool,
}

/// The catalog response from `GET /api/catalog`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogResponse {
    pub mcps: Vec<McpInfo>,
}

/// One matching message within a conversation search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchMessageMatch {
    pub node_id: String,
    pub author: String,
    pub content_snippet: String,
    pub created_at: String,
}

/// One conversation returned by `GET /api/conversations/search` — grouped by conversation (not
/// one flat row per match), so a query hitting several messages in the same conversation shows
/// all of them with their own snippets rather than collapsing to a single row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationSearchResult {
    pub conversation_id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub created_at: String,
    pub matches: Vec<SearchMessageMatch>,
}

/// Response shape from `GET /api/conversations/search`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationSearchResponse {
    pub results: Vec<ConversationSearchResult>,
    /// Count of matching conversations found (the server scans everything before truncating to
    /// the requested `limit`, so this reflects the true total, not just what's returned).
    pub total_found: usize,
}

// ──────────────────────────────────────────────────────────────
// API errors
// ──────────────────────────────────────────────────────────────

/// Standard error body returned by all non-2xx API responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

// ──────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ChatEvent ─────────────────────────────────────────────

    #[test]
    fn chat_event_serialization_round_trips() {
        let event = ChatEvent::Token {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ChatEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ChatEvent::Token { text } if text == "hello"));
    }

    #[test]
    fn tool_event_serialization() {
        let event = ChatEvent::Tool {
            name: "vault:read".into(),
            args: serde_json::json!({"path": "foo.md"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("vault:read"));
        assert!(json.contains("foo.md"));
    }

    #[test]
    fn session_event_serialization() {
        let event = ChatEvent::Session {
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ChatEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ChatEvent::Session { .. }));
    }

    #[test]
    fn tool_result_event_serialization() {
        let event = ChatEvent::ToolResult {
            name: "search".into(),
            ok: true,
            preview: "3 results".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ChatEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ChatEvent::ToolResult { name, ok: true, .. } if name == "search"));
    }

    #[test]
    fn done_event_serialization() {
        let event = ChatEvent::Done;
        let json = serde_json::to_string(&event).unwrap();
        let back: ChatEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ChatEvent::Done));
    }

    #[test]
    fn failed_event_serialization() {
        let event = ChatEvent::Failed {
            message: "connection lost".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ChatEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ChatEvent::Failed { message } if message == "connection lost"));
    }

    #[test]
    fn all_chat_event_variants_round_trip() {
        let events = vec![
            ChatEvent::Session { id: "01ABC".into() },
            ChatEvent::Token { text: "hi".into() },
            ChatEvent::Tool {
                name: "t".into(),
                args: serde_json::json!({}),
            },
            ChatEvent::ToolResult {
                name: "t".into(),
                ok: true,
                preview: "ok".into(),
            },
            ChatEvent::Done,
            ChatEvent::Failed {
                message: "err".into(),
            },
        ];
        for e in events {
            let json = serde_json::to_string(&e).unwrap();
            let _back: ChatEvent = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn chat_response_serialization() {
        let resp = ChatResponse {
            reply: "Hello, world!".into(),
            session: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reply, "Hello, world!");
    }

    // ── from_sse_data ─────────────────────────────────────────

    #[test]
    fn from_sse_data_session() {
        let e = ChatEvent::from_sse_data("session", "abc123").unwrap();
        assert!(matches!(e, ChatEvent::Session { id } if id == "abc123"));
    }

    #[test]
    fn from_sse_data_token() {
        let e = ChatEvent::from_sse_data("token", "hello").unwrap();
        assert!(matches!(e, ChatEvent::Token { text } if text == "hello"));
    }

    #[test]
    fn from_sse_data_tool() {
        let data = r#"{"name":"search","args":{"q":"test"}}"#;
        let e = ChatEvent::from_sse_data("tool", data).unwrap();
        assert!(matches!(e, ChatEvent::Tool { name, .. } if name == "search"));
    }

    #[test]
    fn from_sse_data_tool_result() {
        let data = r#"{"name":"search","ok":true,"preview":"3 results"}"#;
        let e = ChatEvent::from_sse_data("tool_result", data).unwrap();
        assert!(matches!(e, ChatEvent::ToolResult { name, ok: true, .. } if name == "search"));
    }

    #[test]
    fn from_sse_data_done() {
        let e = ChatEvent::from_sse_data("done", "").unwrap();
        assert!(matches!(e, ChatEvent::Done));
    }

    #[test]
    fn from_sse_data_failed() {
        let e = ChatEvent::from_sse_data("failed", "connection refused").unwrap();
        assert!(matches!(e, ChatEvent::Failed { message } if message == "connection refused"));
    }

    #[test]
    fn from_sse_data_unknown_event_type_is_benign_empty_token() {
        // Unknown/future event types must NOT error — they become an empty token (no-op).
        let e = ChatEvent::from_sse_data("future_event_type", "some payload").unwrap();
        assert!(matches!(e, ChatEvent::Token { text } if text.is_empty()));
    }

    #[test]
    fn from_sse_data_tool_malformed_json_returns_err() {
        let result = ChatEvent::from_sse_data("tool", "not valid json");
        assert!(result.is_err(), "expected Err for malformed tool JSON");
    }

    #[test]
    fn from_sse_data_tool_result_malformed_json_returns_err() {
        let result = ChatEvent::from_sse_data("tool_result", "{broken}");
        assert!(
            result.is_err(),
            "expected Err for malformed tool_result JSON"
        );
    }

    // ── DaemonStatus ──────────────────────────────────────────

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
        assert_eq!(back.reactions_seen, 42);
        assert_eq!(back.model_name, Some("deepseek-chat".into()));
    }

    #[test]
    fn daemon_status_server_shape_no_optional_fields() {
        // Simulates the server's current json!() output — minimal fields, no model_name etc.
        let json = serde_json::json!({
            "running": true,
            "vault_path": "/v",
            "uptime_seconds": 100,
            "watcher_active": false,
            "dispatcher_attached": false,
            "orchestrator_attached": false,
            "reactions_seen": 0,
            "chat_tools": 3,
            "chat_tool_names": ["a", "b", "c"]
        });
        let status: DaemonStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status.model_name, None);
        assert_eq!(status.token_usage_total, None);
        assert_eq!(status.context_window, None);
        assert_eq!(status.chat_tools, 3);
    }

    #[test]
    fn daemon_status_missing_chat_tools_defaults_to_zero() {
        // Older server that doesn't emit chat_tools yet.
        let json = serde_json::json!({
            "running": true,
            "vault_path": "/v",
            "watcher_active": false,
            "dispatcher_attached": false,
            "orchestrator_attached": false,
            "reactions_seen": 0
        });
        let status: DaemonStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status.chat_tools, 0);
        assert!(status.chat_tool_names.is_empty());
        assert_eq!(status.uptime_seconds, 0);
    }

    // ── ReactionEvent / ReactionOutcome ───────────────────────

    #[test]
    fn reaction_event_roundtrip() {
        let ev = ReactionEvent {
            event_type: "file_changed".into(),
            timestamp: "2025-06-25T12:00:00Z".into(),
            source: "watcher".into(),
            correlation_id: "abc-123".into(),
            path: Some("/docs/notes.md".into()),
            outcome: ReactionOutcome::Observed,
        };
        let json = serde_json::to_value(&ev).unwrap();
        let back: ReactionEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back.event_type, "file_changed");
        assert_eq!(back.outcome, ReactionOutcome::Observed);
    }

    #[test]
    fn reaction_outcome_deserializes_from_snake_case_strings() {
        let o: ReactionOutcome = serde_json::from_str("\"observed\"").unwrap();
        assert_eq!(o, ReactionOutcome::Observed);
        let d: ReactionOutcome = serde_json::from_str("\"decided\"").unwrap();
        assert_eq!(d, ReactionOutcome::Decided);
        let a: ReactionOutcome = serde_json::from_str("\"acted\"").unwrap();
        assert_eq!(a, ReactionOutcome::Acted);
    }

    #[test]
    fn reaction_outcome_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&ReactionOutcome::Observed).unwrap(),
            "\"observed\""
        );
        assert_eq!(
            serde_json::to_string(&ReactionOutcome::Decided).unwrap(),
            "\"decided\""
        );
        assert_eq!(
            serde_json::to_string(&ReactionOutcome::Acted).unwrap(),
            "\"acted\""
        );
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
        let ev: ReactionEvent = serde_json::from_value(json).unwrap();
        assert_eq!(ev.path, None);
        assert_eq!(ev.outcome, ReactionOutcome::Observed);
    }

    // ── ConvHeader ────────────────────────────────────────────

    #[test]
    fn conv_header_roundtrip_with_parent() {
        let h = ConvHeader {
            id: "c1".into(),
            title: Some("test conversation".into()),
            created_at: "2025-06-25T12:00:00Z".into(),
            parent_conversation: Some("c0".into()),
            spawned_by: Some("msg-5".into()),
        };
        let json = serde_json::to_value(&h).unwrap();
        let back: ConvHeader = serde_json::from_value(json).unwrap();
        assert_eq!(back.id, "c1");
        assert_eq!(back.parent_conversation, Some("c0".into()));
    }

    #[test]
    fn conv_header_missing_parent_fields_default_to_none() {
        let json =
            serde_json::json!({"id": "c2", "title": "plain", "created_at": "2025-06-25T12:00:00Z"});
        let h: ConvHeader = serde_json::from_value(json).unwrap();
        assert_eq!(h.parent_conversation, None);
        assert_eq!(h.spawned_by, None);
    }

    #[test]
    fn conv_header_default_is_all_empty() {
        let h = ConvHeader::default();
        assert!(h.id.is_empty());
        assert!(h.title.is_none());
        assert!(h.parent_conversation.is_none());
    }

    // ── ChatMessage ───────────────────────────────────────────

    #[test]
    fn chat_message_roundtrip_with_tool_calls() {
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
        let tc = back.tool_calls.unwrap();
        assert_eq!(tc[0]["function"]["name"], "search");
    }

    #[test]
    fn chat_message_missing_tool_fields_defaults() {
        let json = serde_json::json!({"role": "user", "content": "hello"});
        let msg: ChatMessage = serde_json::from_value(json).unwrap();
        assert_eq!(msg.tool_calls, None);
        assert_eq!(msg.tool_call_id, None);
    }

    #[test]
    fn conversation_history_response_roundtrip() {
        let resp = ConversationHistoryResponse {
            messages: vec![
                ChatMessage {
                    role: "user".into(),
                    content: "hello".into(),
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "assistant".into(),
                    content: "hi there".into(),
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
        };
        let json = serde_json::to_value(&resp).unwrap();
        let back: ConversationHistoryResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[0].role, "user");
        assert_eq!(back.messages[1].content, "hi there");
    }

    // ── VaultInfo / ApiError ──────────────────────────────────

    #[test]
    fn vault_info_roundtrip() {
        let v = VaultInfo {
            root: "/vault".into(),
            note_count: 42,
            watcher_active: true,
        };
        let json = serde_json::to_value(&v).unwrap();
        let back: VaultInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back.root, "/vault");
        assert_eq!(back.note_count, 42);
        assert!(back.watcher_active);
    }

    #[test]
    fn api_error_roundtrip() {
        let e = ApiError {
            error: "chat disabled".into(),
        };
        let json = serde_json::to_value(&e).unwrap();
        let back: ApiError = serde_json::from_value(json).unwrap();
        assert_eq!(back.error, "chat disabled");
    }

    // ── McpInfo / CatalogResponse ─────────────────────────────

    #[test]
    fn mcp_info_roundtrip() {
        let info = McpInfo {
            name: "vault".into(),
            description: "File system access".into(),
            consequence: "reversible".into(),
            tool_count: 3,
            tool_names: vec!["read".into(), "write".into(), "search".into()],
            provenance: Some("config/topology.toml".into()),
            visible_to_main_agent: true,
            visible_to_dispatcher: false,
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: McpInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "vault");
        assert_eq!(back.tool_count, 3);
        assert_eq!(back.provenance, Some("config/topology.toml".into()));
        assert!(back.visible_to_main_agent);
        assert!(!back.visible_to_dispatcher);
    }

    #[test]
    fn mcp_info_provenance_skipped_when_none() {
        let info = McpInfo {
            name: "vault".into(),
            description: "desc".into(),
            consequence: "read_only".into(),
            tool_count: 0,
            tool_names: vec![],
            provenance: None,
            visible_to_main_agent: false,
            visible_to_dispatcher: false,
        };
        let json = serde_json::to_string(&info).unwrap();
        // provenance: None should be omitted entirely from JSON
        assert!(
            !json.contains("provenance"),
            "None provenance should not appear in JSON"
        );
        let back: McpInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.provenance, None);
    }

    #[test]
    fn mcp_info_default_tool_count_and_names() {
        // Simulates server sending McpInfo without tool_count / tool_names
        let json = serde_json::json!({
            "name": "tasks",
            "description": "task manager",
            "consequence": "reversible"
        });
        let info: McpInfo = serde_json::from_value(json).unwrap();
        assert_eq!(info.tool_count, 0);
        assert!(info.tool_names.is_empty());
    }

    #[test]
    fn mcp_info_visibility_flags_default_to_false_when_absent() {
        // An older server that doesn't emit visible_to_main_agent/visible_to_dispatcher yet.
        let json = serde_json::json!({
            "name": "tasks",
            "description": "task manager",
            "consequence": "reversible"
        });
        let info: McpInfo = serde_json::from_value(json).unwrap();
        assert!(!info.visible_to_main_agent);
        assert!(!info.visible_to_dispatcher);
    }

    #[test]
    fn catalog_response_roundtrip() {
        let catalog = CatalogResponse {
            mcps: vec![McpInfo {
                name: "vault".into(),
                description: "File system".into(),
                consequence: "reversible".into(),
                tool_count: 2,
                tool_names: vec!["read".into(), "write".into()],
                provenance: None,
                visible_to_main_agent: true,
                visible_to_dispatcher: true,
            }],
        };
        let json = serde_json::to_string(&catalog).unwrap();
        let back: CatalogResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mcps.len(), 1);
        assert_eq!(back.mcps[0].name, "vault");
    }

    // ── ConversationSearchResult / ConversationSearchResponse ─────────────────

    #[test]
    fn conversation_search_result_roundtrip() {
        let resp = ConversationSearchResponse {
            results: vec![ConversationSearchResult {
                conversation_id: "01ABC".into(),
                title: Some("My Chat".into()),
                created_at: "2026-01-01T00:00:00Z".into(),
                matches: vec![SearchMessageMatch {
                    node_id: "01DEF".into(),
                    author: "user".into(),
                    content_snippet: "…hello world…".into(),
                    created_at: "2026-01-01T00:01:00Z".into(),
                }],
            }],
            total_found: 1,
        };
        let json = serde_json::to_value(&resp).unwrap();
        let back: ConversationSearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.results.len(), 1);
        assert_eq!(back.results[0].matches[0].author, "user");
        assert_eq!(back.total_found, 1);
    }

    #[test]
    fn conversation_search_result_missing_title_defaults_to_none() {
        let json = serde_json::json!({
            "conversation_id": "x",
            "created_at": "2026-01-01T00:00:00Z",
            "matches": []
        });
        let r: ConversationSearchResult = serde_json::from_value(json).unwrap();
        assert_eq!(r.title, None);
    }

    #[test]
    fn conversation_search_result_groups_multiple_matches_per_conversation() {
        let result = ConversationSearchResult {
            conversation_id: "01ABC".into(),
            title: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            matches: vec![
                SearchMessageMatch {
                    node_id: "01DEF".into(),
                    author: "user".into(),
                    content_snippet: "first match".into(),
                    created_at: "2026-01-01T00:01:00Z".into(),
                },
                SearchMessageMatch {
                    node_id: "01GHI".into(),
                    author: "assistant".into(),
                    content_snippet: "second match".into(),
                    created_at: "2026-01-01T00:02:00Z".into(),
                },
            ],
        };
        assert_eq!(result.matches.len(), 2);
    }
}
