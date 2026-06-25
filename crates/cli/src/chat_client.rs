//! The `liberado chat` terminal client — a thin REPL over a running daemon server's chat API.
//!
//! This module embeds **no** agent logic: no `ChatSessions`, no provider, no store. It is purely an
//! HTTP/SSE client of `POST /api/chat/stream` (the shared client contract — see `docs/interface.md`),
//! streaming the answer back token-by-token and printing tool activity as it happens. It is the first
//! *native* (`reqwest`/SSE) client of that API, seeding the future TUI: same bytes, same parser.
//!
//! The conversation lives server-side. The client only holds the current session id — learned from
//! the `session` event the server emits first — and sends it back on each subsequent turn so the
//! daemon continues the same conversation.

use futures::StreamExt;
use std::io::Write as _;
use tokio::io::{AsyncBufReadExt, BufReader, stdin};

/// Where to find the daemon server when `LIBERADO_SERVER` is unset.
const DEFAULT_SERVER: &str = "http://127.0.0.1:4201";

/// Run the interactive chat client against a running daemon server. `resume` is an optional
/// conversation id to continue; without it, the first message starts a new conversation (the id is
/// learned from the `session` event and reused for the rest of the REPL).
pub async fn run(resume: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::var("LIBERADO_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_string());
    let endpoint = format!("{base}/api/chat/stream");
    let client = reqwest::Client::new();
    let mut session: Option<String> = resume;

    println!("liberado chat -> {base}");
    println!("type 'exit' or Ctrl-D to quit");
    if let Some(id) = &session {
        println!("(resuming session {id})");
    }

    // stdout is the conversation; stderr carries logs and errors. Read prompts one line at a time.
    let mut lines = BufReader::new(stdin()).lines();
    loop {
        print!("> ");
        std::io::stdout().flush()?;

        let line = match lines.next_line().await? {
            Some(line) => line,
            None => break, // EOF (Ctrl-D)
        };
        let message = line.trim();
        if message.is_empty() {
            continue;
        }
        if message == "exit" || message == "quit" {
            break;
        }

        if let Err(err) = turn(&client, &endpoint, &base, &mut session, message).await {
            // A turn-level error (not a connection error, which `turn` reports itself) — surface it
            // and keep the REPL alive.
            eprintln!("\n[error] {err}");
        }
    }
    Ok(())
}

/// One REPL turn: POST the message (with the current session, if any), stream the SSE response, and
/// print it. Connection and HTTP-status problems are reported and swallowed so the loop survives;
/// only an unexpected streaming error bubbles up.
async fn turn(
    client: &reqwest::Client,
    endpoint: &str,
    base: &str,
    session: &mut Option<String>,
    message: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = serde_json::json!({ "message": message, "session": session });

    let response = match client.post(endpoint).json(&body).send().await {
        Ok(response) => response,
        Err(_) => {
            println!(
                "could not reach the daemon at {base} — start it with: liberado serve <vault>"
            );
            return Ok(());
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        println!("[error] server returned {status}: {body}");
        return Ok(());
    }

    let mut decoder = SseDecoder::default();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let text = String::from_utf8_lossy(&chunk);
        for event in decoder.push(&text) {
            if dispatch(&event, session) {
                return Ok(()); // turn complete (`done` or `failed`)
            }
        }
    }
    // Stream ended without an explicit terminator — still end the turn cleanly.
    println!();
    Ok(())
}

/// Render one SSE event to the terminal and fold any session id back into `session`. Returns `true`
/// when the event terminates the turn (`done`/`failed`).
fn dispatch(event: &SseEvent, session: &mut Option<String>) -> bool {
    match event.event.as_str() {
        "session" => {
            *session = Some(event.data.clone());
            false
        }
        "token" => {
            print!("{}", event.data);
            let _ = std::io::stdout().flush();
            false
        }
        "tool" => {
            match serde_json::from_str::<serde_json::Value>(&event.data) {
                Ok(call) => println!(
                    "\n  [tool] {}({})",
                    field(&call, "name"),
                    truncate(&field(&call, "args"), 200)
                ),
                Err(_) => println!("\n  [tool] {}", event.data),
            }
            false
        }
        "tool_result" => {
            match serde_json::from_str::<serde_json::Value>(&event.data) {
                Ok(result) => println!(
                    "\n  [tool] {} {} {}",
                    field(&result, "name"),
                    if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                        "ok"
                    } else {
                        "err"
                    },
                    truncate(&field(&result, "preview"), 200)
                ),
                Err(_) => println!("\n  [tool] {}", event.data),
            }
            false
        }
        "done" => {
            println!();
            true
        }
        "failed" => {
            eprintln!("\n[error] {}", event.data);
            true
        }
        _ => false, // unknown event type — ignore
    }
}

/// Read a string field from a tool event's JSON, falling back to empty when absent or non-string.
fn field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Clamp a preview string for terminal legibility, appending an ellipsis marker when cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}...")
}

/// A decoded Server-Sent Event. `event` defaults to `"message"` when the block has no `event:` line.
#[derive(Debug, PartialEq)]
struct SseEvent {
    event: String,
    data: String,
}

/// An incremental SSE parser: feed it response-body chunks via [`push`](Self::push) and it returns
/// whatever complete events have arrived, buffering any trailing partial event for the next chunk.
/// Not a dependency — the contract we consume is small and stable.
#[derive(Default)]
struct SseDecoder {
    buf: String,
}

impl SseDecoder {
    /// Append a chunk and split off every complete event (an event ends at a blank line). A trailing
    /// partial event stays in the buffer until its terminating blank line arrives.
    fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        // Normalise CRLF so blank-line detection and line splitting are newline-only.
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

/// Parse one SSE event block (the lines up to and including its terminating blank line) into an
/// [`SseEvent`]. Returns `None` if the block holds no `data:`/`event:` lines (e.g. only comments).
fn parse_block(block: &str) -> Option<SseEvent> {
    let mut event_type: Option<String> = None;
    let mut data_lines: Vec<&str> = Vec::new();
    let mut saw_field = false;

    for line in block.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue; // blank terminator or comment
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
        // First chunk cuts the event mid-data — no complete event yet.
        assert!(decoder.push("event: token\ndata: Hel").is_empty());
        // The remainder completes it.
        let events = decoder.push("lo\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "token");
        assert_eq!(events[0].data, "Hello");
    }

    #[test]
    fn comment_line_is_ignored_and_does_not_corrupt_following_event() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(":no session\n\nevent: token\ndata: Hi\n\n");
        // The comment-only block yields no event; the real one parses cleanly.
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
}
