//! Native (non-WASM) SSE framing decoder.
//!
//! Gated behind `#[cfg(not(target_arch = "wasm32"))]` in `lib.rs` so the WebUI can depend on
//! `chat-client-contract` without pulling in `tokio`/`futures`.
//!
//! This module used to also declare a `ChatClient` trait (`send`/`stream`) meant to be one shared
//! client implementation for the TUI and `liberado chat` CLI. Neither ever implemented it — each
//! hand-rolled its own POST + SSE loop directly against `reqwest` (`crates/cli/src/chat_client.rs`,
//! `crates/tui/src/effects.rs`), because their actual needs diverge past the point a `send`/`stream`
//! trait usefully captures: the CLI drives a blocking terminal REPL, the TUI feeds a non-blocking
//! render loop via its own action/effect channels. Removed 2026-07-05
//! (`docs/future-work/archive/hygiene-audit-2026-07-05.md`) rather than force an implementation neither client
//! actually wanted. [`SseDecoder`] below (SSE framing) is the real shared boundary both clients use
//! today. [`crate::wire::SessionEvent::from_sse_data`] (typed payload decoding — the converged
//! chat + goal-session vocabulary, 2026-07-11) is used on top of it by the TUI
//! (`liberado-tui`'s `sse::ToAction`) and the CLI (`crates/cli/src/chat_client.rs`).

// ── Incremental SSE parser ───────────────────────────────────────────────────
//
// Feed it `reqwest` response byte-stream chunks; it returns complete `SseEvent`s as they
// arrive, buffering any trailing partial event for the next chunk. Native transport only
// (browser `EventSource` handles its own framing) — shared by every `reqwest`-based client
// (the TUI and the `liberado chat` CLI), which previously each carried their own copy.
//
// The SSE contract parsed here is documented in `docs/spec/reference/api.md`: events are
// `session`, `token`, `tool`, `tool_result`, `done`, `failed`.

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
        self.buf
            .push_str(&chunk.replace("\r\n", "\n").replace('\r', "\n"));

        let mut events = Vec::new();
        while let Some(idx) = self.buf.find("\n\n") {
            let block: String = self.buf.drain(..idx).collect();
            self.buf.drain(..2);
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
        if line.starts_with(':') {
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
mod sse_tests {
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
    }

    #[test]
    fn tool_finished_event() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: tool_finished\ndata: {\"name\":\"search\",\"ok\":true,\"result_preview\":\"3 results\"}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "tool_finished");
        assert!(events[0].data.contains("search"));
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

#[test]
fn comment_only_block_yields_no_event() {
    let mut d = SseDecoder::default();
    let events = d.push(": a comment only\n\n");
    assert!(events.is_empty());
}

#[test]
fn empty_line_between_events_separates_them() {
    let mut d = SseDecoder::default();
    let events = d.push("event: token\ndata: hello\n\nevent: token\ndata: world\n\n");
    assert_eq!(events.len(), 2);
}
