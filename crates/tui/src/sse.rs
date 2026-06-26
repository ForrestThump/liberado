//! Incremental Server-Sent Events parser.
//!
//! Feed it `reqwest` response byte-stream chunks; it returns complete `SseEvent`s as
//! they arrive, buffering any trailing partial event for the next chunk. This is the
//! same pattern as `crates/cli/chat_client.rs`'s inline decoder, extracted so it can
//! be unit-tested independently and reused by both the chat client and the TUI.
//!
//! The SSE contract consumed here is documented in `docs/interface.md`:
//! events are `session`, `token`, `tool`, `tool_result`, `done`, `failed`.

/// A decoded Server-Sent Event. `event` defaults to `"message"` when the block has no
/// `event:` line (per the SSE spec).
///
/// Call [`to_action()`](SseEvent::to_action) to convert into an [`Action`] for
/// [`App::update()`](crate::app::App::update).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

use crate::app::Action;

impl SseEvent {
    /// Convert this SSE event into the corresponding [`Action`] variant.
    /// Handles all 6 event types from `docs/interface.md`:
    /// `session`, `token`, `tool`, `tool_result`, `done`, `failed`.
    /// Unknown event types default to `Ok(Action::SseToken(String::new()))`.
    /// Returns `Err` with a description when `tool`/`tool_result` JSON is malformed.
    pub fn to_action(&self) -> Result<Action, String> {
        match self.event.as_str() {
            "session" => Ok(Action::SseSession(self.data.clone())),
            "token" => Ok(Action::SseToken(self.data.clone())),
            "tool" => {
                let v: serde_json::Value = serde_json::from_str(&self.data)
                    .map_err(|e| format!("malformed tool SSE JSON ({e}): {}", self.data))?;
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("?")
                    .to_string();
                let args = v
                    .get("args")
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(Action::SseTool { name, args })
            }
            "tool_result" => {
                let v: serde_json::Value = serde_json::from_str(&self.data)
                    .map_err(|e| format!("malformed tool_result SSE JSON ({e}): {}", self.data))?;
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("?")
                    .to_string();
                let ok = v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
                let preview = v
                    .get("preview")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(Action::SseToolResult { name, ok, preview })
            }
            "done" => Ok(Action::SseDone),
            "failed" => Ok(Action::SseFailed(self.data.clone())),
            _ => Ok(Action::SseToken(String::new())),
        }
    }
}

/// An incremental SSE parser. Create with [`SseDecoder::default()`], feed chunks via
/// [`push`](Self::push), and drain the resulting complete events. A trailing partial
/// event stays in the buffer until its terminating blank line arrives in a later chunk.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: String,
}

impl SseDecoder {
    /// Append a chunk and return every complete event that can be split off. A
    /// trailing partial event (no terminating blank line yet) stays buffered.
    pub fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buf
            .push_str(&chunk.replace("\r\n", "\n").replace('\r', "\n"));

        let mut events = Vec::new();
        while let Some(idx) = self.buf.find("\n\n") {
            let block: String = self.buf.drain(..idx + 2).collect();
            if let Some(event) = parse_block(&block) {
                events.push(event);
            }
        }
        events
    }
}

/// Parse one SSE event block (the lines up to and including its terminating blank
/// line) into an [`SseEvent`]. Returns `None` if the block holds no `data:`/`event:`
/// lines (e.g. only comments).
fn parse_block(block: &str) -> Option<SseEvent> {
    let mut event_type: Option<String> = None;
    let mut data_lines: Vec<&str> = Vec::new();
    let mut saw_field = false;

    for line in block.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_type = Some(strip_one_space(value).to_string());
            saw_field = true;
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(strip_one_space(value));
            saw_field = true;
        }
    }

    if !saw_field {
        return None;
    }
    Some(SseEvent {
        event: event_type.unwrap_or_else(|| "message".to_string()),
        data: data_lines.join("\n"),
    })
}

/// Strip a single optional leading space from an SSE field value (per the spec).
fn strip_one_space(value: &str) -> &str {
    value.strip_prefix(' ').unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_event_in_one_chunk() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: token\ndata: Hello\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: "token".to_string(),
                data: "Hello".to_string(),
            }]
        );
    }

    #[test]
    fn multi_line_data_joins_with_newline() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: token\ndata: line1\ndata: line2\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[test]
    fn event_split_across_pushes_assembles() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push("event: token\ndata: Hel").is_empty());
        let events = decoder.push("lo\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "token");
        assert_eq!(events[0].data, "Hello");
    }

    #[test]
    fn comment_line_is_ignored() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(":no session\n\nevent: token\ndata: Hi\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "token");
        assert_eq!(events[0].data, "Hi");
    }

    #[test]
    fn handles_crlf_line_endings() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: done\r\ndata: \r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "done");
    }

    #[test]
    fn unknown_event_type_defaults_to_message() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("data: some payload\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "message");
        assert_eq!(events[0].data, "some payload");
    }

    #[test]
    fn session_event() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: session\ndata: c1a2b3d4\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "session");
        assert_eq!(events[0].data, "c1a2b3d4");
    }

    #[test]
    fn tool_event_with_json_data() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(
            "event: tool\ndata: {\"name\":\"search\",\"args\":\"{\\\"q\\\":\\\"test\\\"}\"}\n\n",
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "tool");
        assert_eq!(
            events[0].data,
            "{\"name\":\"search\",\"args\":\"{\\\"q\\\":\\\"test\\\"}\"}"
        );
        let action = events[0].to_action().unwrap();
        assert!(matches!(action, Action::SseTool { name, .. } if name == "search"));
    }

    #[test]
    fn tool_result_event() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: tool_result\ndata: {\"name\":\"search\",\"ok\":true,\"preview\":\"3 results\"}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "tool_result");
        assert!(events[0].data.contains("search"));
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
            err.contains("malformed tool SSE JSON"),
            "error should mention malformed JSON: {err}"
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

    #[test]
    fn failed_event() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: failed\ndata: connection refused\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "failed");
        assert_eq!(events[0].data, "connection refused");
    }

    #[test]
    fn done_event() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: done\ndata: \n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "done");
    }

    #[test]
    fn multiple_events_in_single_chunk() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: token\ndata: Hello\n\nevent: token\ndata: World\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "Hello");
        assert_eq!(events[1].data, "World");
    }

    #[test]
    fn comments_only_block_returns_nothing() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(": a comment\n: another comment\n\n");
        assert!(events.is_empty());
    }

    #[test]
    fn data_no_space_after_colon() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: token\ndata:Hello\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "Hello");
    }

    #[test]
    fn bare_cr_line_endings() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: token\rdata: Hi\r\r");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "token");
        assert_eq!(events[0].data, "Hi");
    }
}
