//! TUI-specific conversion from the shared SSE decoder's events into [`Action`].
//!
//! The incremental parser itself (`SseDecoder`/`SseEvent`) lives in
//! `chat_client_contract::native` — it's shared with the `liberado chat` CLI client, which
//! previously carried its own separate copy. Only the conversion to this crate's `Action` enum
//! is TUI-specific and stays here.

pub use chat_client_contract::native::{SseDecoder, SseEvent};

use chat_client_contract::ChatEvent;

use crate::app::Action;

/// Convert a decoded [`SseEvent`] into the corresponding [`Action`] variant for
/// [`App::update()`](crate::app::App::update).
///
/// A local trait (rather than an inherent `impl SseEvent`) because `SseEvent` is defined in
/// `chat_client_contract`, not this crate — Rust's orphan rules only allow implementing a
/// *local* trait for a foreign type.
pub trait ToAction {
    /// Delegates JSON parsing to [`ChatEvent::from_sse_data`] (the shared wire helper).
    ///
    /// **Semantics preserved from the original hand-rolled implementation:**
    /// - Unknown/future event types → `Ok(Action::SseToken(String::new()))` (benign no-op)
    /// - Malformed JSON for a *known* `tool`/`tool_result` event → `Err(description)`
    fn to_action(&self) -> Result<Action, String>;
}

impl ToAction for SseEvent {
    fn to_action(&self) -> Result<Action, String> {
        // from_sse_data maps unknown event types to ChatEvent::Token { text: "" } (benign),
        // and returns Err only for malformed JSON on known tool/tool_result events.
        match ChatEvent::from_sse_data(&self.event, &self.data) {
            Ok(event) => Ok(match event {
                ChatEvent::Session { id } => Action::SseSession(id),
                ChatEvent::Token { text } => Action::SseToken(text),
                ChatEvent::Tool { name, args } => Action::SseTool {
                    name,
                    args: args.to_string(),
                },
                ChatEvent::ToolResult { name, ok, preview } => {
                    Action::SseToolResult { name, ok, preview }
                }
                ChatEvent::Done => Action::SseDone,
                ChatEvent::Failed { message } => Action::SseFailed(message),
            }),
            Err(e) => Err(format!("malformed SSE data ({e}): {}", self.data)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_event_converts_to_sse_tool_action() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(
            "event: tool\ndata: {\"name\":\"search\",\"args\":\"{\\\"q\\\":\\\"test\\\"}\"}\n\n",
        );
        let action = events[0].to_action().unwrap();
        assert!(matches!(action, Action::SseTool { name, .. } if name == "search"));
    }

    #[test]
    fn tool_result_event_converts_to_sse_tool_result_action() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: tool_result\ndata: {\"name\":\"search\",\"ok\":true,\"preview\":\"3 results\"}\n\n");
        let action = events[0].to_action().unwrap();
        assert!(matches!(action, Action::SseToolResult { name, ok: true, .. } if name == "search"));
    }

    #[test]
    fn tool_malformed_json_returns_err() {
        let event = SseEvent {
            event: "tool".to_string(),
            data: "not valid json".to_string(),
        };
        let result = event.to_action();
        assert!(result.is_err(), "expected Err for malformed JSON");
        let err = result.unwrap_err();
        assert!(
            err.contains("malformed SSE data"),
            "error should mention malformed SSE data: {err}"
        );
    }

    #[test]
    fn tool_result_malformed_json_returns_err() {
        let event = SseEvent {
            event: "tool_result".to_string(),
            data: "{broken}".to_string(),
        };
        let result = event.to_action();
        assert!(result.is_err(), "expected Err for malformed JSON");
    }
}
