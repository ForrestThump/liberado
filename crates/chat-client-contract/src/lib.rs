//! Shared SSE contract for Liberado chat clients.
//! Every client (TUI, CLI, WebUI) depends only on this crate to talk to `liberado serve`.
//! The server also depends on it for the types it emits.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use ulid::Ulid;

/// A streaming chat event — maps 1:1 with the SSE events produced by the server's
/// `/api/chat/stream` endpoint.
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
    Failed {
        message: String,
    },
}

/// The error type for chat client operations.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("deserialization: {0}")]
    Deserialize(String),
    #[error("chat disabled: {0}")]
    Disabled(String),
}

/// A chat client trait — implemented by HTTP/SSE clients that talk to `liberado serve`.
#[async_trait]
pub trait ChatClient {
    /// Send a message non-streaming, returning the reply and session id.
    async fn send(&self, message: &str, session: Option<Ulid>) -> Result<ChatResponse, ChatError>;

    /// Send a message and stream events back.
    async fn stream(
        &self,
        message: &str,
        session: Option<Ulid>,
    ) -> Result<
        Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, ChatError>> + Send>>,
        ChatError,
    >;
}

/// A non-streaming chat response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub reply: String,
    pub session: String,
}

/// Information about an MCP server returned by GET /api/catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInfo {
    pub name: String,
    pub description: String,
    pub consequence: String,
    /// Number of tools this MCP exposes.
    pub tool_count: usize,
    /// Names of tools this MCP exposes.
    pub tool_names: Vec<String>,
}

/// The catalog response from GET /api/catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogResponse {
    pub mcps: Vec<McpInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn all_variants_round_trip() {
        let events = vec![
            ChatEvent::Session {
                id: "01ABC".into(),
            },
            ChatEvent::Token {
                text: "hi".into(),
            },
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

    #[test]
    fn mcp_info_serialization() {
        let info = McpInfo {
            name: "vault".into(),
            description: "File system access".into(),
            consequence: "Reads/writes markdown files".into(),
            tool_count: 3,
            tool_names: vec!["read".into(), "write".into(), "search".into()],
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: McpInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "vault");
        assert_eq!(back.tool_count, 3);
    }

    #[test]
    fn catalog_response_serialization() {
        let catalog = CatalogResponse {
            mcps: vec![McpInfo {
                name: "vault".into(),
                description: "File system".into(),
                consequence: "RW access".into(),
                tool_count: 2,
                tool_names: vec!["read".into(), "write".into()],
            }],
        };
        let json = serde_json::to_string(&catalog).unwrap();
        let back: CatalogResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mcps.len(), 1);
        assert_eq!(back.mcps[0].name, "vault");
    }
}
