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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
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
        todo!("push: normalize CRLF → LF, accumulate in buf, split on double-newline, parse each block")
    }
}

/// Parse one SSE event block (the lines up to and including its terminating blank
/// line) into an [`SseEvent`]. Returns `None` if the block holds no `data:`/`event:`
/// lines (e.g. only comments).
fn parse_block(block: &str) -> Option<SseEvent> {
    todo!("parse_block: iterate lines, collect event: and data: fields, strip optional leading spaces")
}

/// Strip a single optional leading space from an SSE field value (per the spec).
fn strip_one_space(value: &str) -> &str {
    todo!("strip_one_space")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_event_in_one_chunk() {
        todo!("single_event_in_one_chunk")
    }

    #[test]
    fn multi_line_data_joins_with_newline() {
        todo!("multi_line_data_joins_with_newline")
    }

    #[test]
    fn event_split_across_pushes_assembles() {
        todo!("event_split_across_pushes_assembles")
    }

    #[test]
    fn comment_line_is_ignored() {
        todo!("comment_line_is_ignored")
    }

    #[test]
    fn handles_crlf_line_endings() {
        todo!("handles_crlf_line_endings")
    }

    #[test]
    fn unknown_event_type_defaults_to_message() {
        todo!("unknown_event_type_defaults_to_message")
    }
}
