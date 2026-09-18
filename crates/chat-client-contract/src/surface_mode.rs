//! Chat-surface shelf stamp shared by every chat client.
//!
//! Soft client-side taxonomy (`chat` vs `agent`); the kernel does not branch on
//! it. Spec: `docs/spec/architecture/chat-agent-surface-mode.md`.

use serde::{Deserialize, Serialize};

/// Which **chat-surface shelf** a conversation belongs to. The split is a soft
/// client-side taxonomy; the kernel does not branch on it.
///
/// - `Chat` — an open-ended chat (the default).
/// - `Agent` — a long-lived specialist chat, Grok-Bot-style: a curated-hat
///   context with named tools, never terminal. The stamp is create-time
///   (explicit `surface_mode`, or `grant.profile` in a small `agent_profiles`
///   set) — **not** `goal.is_some()`. Spec:
///   `docs/spec/architecture/chat-agent-surface-mode.md`.
///
/// `#[serde(rename_all = "lowercase")]` so the wire is the human-readable
/// string (`"chat"`, `"agent"`); `Default = Chat` is the honest missing-field
/// behavior — old clients, old logs, and unprofiled chats all read as chat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SurfaceMode {
    #[default]
    Chat,
    Agent,
}

impl SurfaceMode {
    /// The wire string — `"chat"` or `"agent"`. Useful for projection paths
    /// that need a `&str` (the chat-lens header builder, log messages).
    pub fn as_str(self) -> &'static str {
        match self {
            SurfaceMode::Chat => "chat",
            SurfaceMode::Agent => "agent",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::ConvHeader;

    #[test]
    fn conv_header_missing_surface_mode_defaults_to_chat() {
        // Pre-stamp rows (and older clients reading a new daemon) omit
        // `surface_mode`; serde must default the field to `Chat`.
        let json = serde_json::json!({"id": "c3", "title": "legacy", "created_at": "2025-06-25T12:00:00Z"});
        let h: ConvHeader = serde_json::from_value(json).unwrap();
        assert_eq!(h.surface_mode, SurfaceMode::Chat);
    }

    #[test]
    fn conv_header_agent_roundtrip_preserves_stamp() {
        let h = ConvHeader {
            id: "c4".into(),
            title: None,
            created_at: "2025-06-25T12:00:00Z".into(),
            parent_conversation: None,
            spawned_by: None,
            surface_mode: SurfaceMode::Agent,
        };
        let json = serde_json::to_value(&h).unwrap();
        assert_eq!(json["surface_mode"], "agent");
        let back: ConvHeader = serde_json::from_value(json).unwrap();
        assert_eq!(back.surface_mode, SurfaceMode::Agent);
    }

    #[test]
    fn as_str_matches_wire_spelling() {
        assert_eq!(SurfaceMode::Chat.as_str(), "chat");
        assert_eq!(SurfaceMode::Agent.as_str(), "agent");
    }

    #[test]
    fn surface_mode_serializes_lowercase() {
        // Pin the wire spelling. The chat-shelf / agent-shelf partitioning
        // code reads it as a string; a rename to PascalCase here would
        // silently leave every old client on the chat shelf.
        assert_eq!(
            serde_json::to_value(SurfaceMode::Chat).unwrap(),
            serde_json::json!("chat")
        );
        assert_eq!(
            serde_json::to_value(SurfaceMode::Agent).unwrap(),
            serde_json::json!("agent")
        );
    }
}
