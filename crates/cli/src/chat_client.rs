//! The `liberado chat` terminal client — a thin REPL over a running daemon server's chat API.
//!
//! This module embeds **no** agent logic: no `ChatSessions`, no provider, no store. It is purely an
//! HTTP/SSE client of `POST /api/chat/stream` (the shared client contract — see `docs/spec/reference/api.md`),
//! streaming the answer back token-by-token and printing tool activity as it happens. It is the first
//! *native* (`reqwest`/SSE) client of that API, seeding the future TUI: same bytes, same parser.
//!
//! The conversation lives server-side. The client only holds the current session id — learned from
//! the `session` event the server emits first — and sends it back on each subsequent turn so the
//! daemon continues the same conversation.

use chat_client_contract::native::{SseDecoder, SseEvent};
use chat_client_contract::{SessionEvent, SessionEventKind};
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
/// when the event terminates the turn (`session_finished`/`failed`). Decoding goes through the
/// shared converged vocabulary (`SessionEvent::from_sse_data`) — the same decoder the TUI uses.
fn dispatch(event: &SseEvent, session: &mut Option<String>) -> bool {
    let decoded = match SessionEvent::from_sse_data(&event.event, &event.data) {
        Ok(decoded) => decoded,
        Err(_) => {
            println!("\n  [?] {}", event.data);
            return false;
        }
    };
    match decoded.kind {
        SessionEventKind::Session { id } => {
            *session = Some(id);
            false
        }
        SessionEventKind::Token { text } => {
            print!("{text}");
            let _ = std::io::stdout().flush();
            false
        }
        SessionEventKind::ToolStarted { name, args_preview } => {
            println!("\n  [tool] {name}({})", truncate(&args_preview, 200));
            false
        }
        SessionEventKind::ToolFinished {
            name,
            ok,
            result_preview,
        } => {
            println!(
                "\n  [tool] {name} {} {}",
                if ok { "ok" } else { "err" },
                truncate(&result_preview, 200)
            );
            false
        }
        SessionEventKind::SessionFinished { .. } => {
            println!();
            true
        }
        SessionEventKind::Failed { message } => {
            eprintln!("\n[error] {message}");
            true
        }
        // A chat turn offered a specialist session — print the join hint (the `liberado chat` CLI
        // can't focus-switch, but the daemon-side session is joinable from the TUI / API by id).
        SessionEventKind::SessionOffered {
            id,
            domain,
            description,
        } => {
            println!("\n  ▸ {domain} session offered: {description}  (join: {id})");
            false
        }
        // Goal-session-only kinds — a chat turn doesn't emit these today; ignore quietly.
        SessionEventKind::SessionStarted { .. }
        | SessionEventKind::RoleStarted { .. }
        | SessionEventKind::RoleFinished { .. }
        | SessionEventKind::Progress { .. }
        | SessionEventKind::AwaitingInput { .. }
        | SessionEventKind::HumanInput { .. }
        | SessionEventKind::ValidationFinished { .. }
        | SessionEventKind::CriticVerdict { .. }
        | SessionEventKind::FileChanged { .. }
        | SessionEventKind::Checkpoint { .. }
        | SessionEventKind::LoopGuard { .. } => false,
    }
}

/// Clamp a preview string for terminal legibility, appending an ellipsis marker when cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}...")
}

#[cfg(test)]
mod tests {
    use super::{dispatch, truncate};
    use chat_client_contract::native::SseEvent;

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
        }
    }

    // ── truncate ────────────────────────────────────────────────────────

    /// Short text passes through whole; the cut is on *characters*, so multi-byte text is not
    /// split mid-codepoint the way a byte slice would.
    #[test]
    fn truncate_passes_short_text_through() {
        assert_eq!(truncate("hello", 200), "hello");
        assert_eq!(truncate("", 200), "");
        assert_eq!(truncate("exactly", 7), "exactly");
    }

    #[test]
    fn truncate_clamps_and_appends_an_ellipsis() {
        assert_eq!(truncate("hello world", 5), "hello...");
        assert_eq!(truncate("hello", 0), "...");
        // Characters, not bytes: 5 CJK chars = 15 bytes, still cut at 5 chars.
        assert_eq!(truncate("中中中中中中", 3), "中中中...");
    }

    // ── dispatch ────────────────────────────────────────────────────────

    /// The `session` event is the conversation's identity: it folds the id back into the REPL's
    /// `session` so the next turn continues the same conversation, and does not end the turn.
    #[test]
    fn session_event_updates_the_session() {
        let mut session = None;
        assert!(!dispatch(&ev("session", "01HZABC"), &mut session));
        assert_eq!(session.as_deref(), Some("01HZABC"));
    }

    /// Token and tool events render but do not end the turn.
    #[test]
    fn mid_turn_events_do_not_end_the_turn() {
        let mut session = Some("01A".into());
        assert!(!dispatch(&ev("token", "hello"), &mut session));
        assert!(!dispatch(
            &ev("tool_started", r#"{"name":"search","args_preview":"q=x"}"#),
            &mut session
        ));
        assert!(!dispatch(
            &ev(
                "tool_finished",
                r#"{"name":"search","ok":true,"result_preview":"done"}"#
            ),
            &mut session
        ));
        assert_eq!(
            session.as_deref(),
            Some("01A"),
            "session must survive a turn"
        );
    }

    /// `session_finished` and `failed` are the turn's terminators — the REPL returns once either
    /// arrives, which is what lets the next prompt appear.
    #[test]
    fn terminators_end_the_turn() {
        let mut session = None;
        assert!(dispatch(
            &ev("session_finished", r#"{"status":"done","summary":""}"#),
            &mut session
        ));
        assert!(dispatch(
            &ev("failed", r#"{"message":"boom"}"#),
            &mut session
        ));
    }

    /// A session offer is informational — the chat CLI cannot focus-switch, so it prints the join
    /// hint and keeps streaming.
    #[test]
    fn session_offer_does_not_end_the_turn() {
        let mut session = None;
        assert!(!dispatch(
            &ev(
                "session_offered",
                r#"{"id":"s2","domain":"coding","description":"a goal"}"#
            ),
            &mut session
        ));
    }

    /// Goal-session-only event kinds are ignored by a chat turn (they are not emitted today) but
    /// must not end the turn or panic.
    #[test]
    fn goal_only_kinds_are_ignored() {
        let mut session = None;
        for event in [
            ("progress", r#"{"message":"thinking"}"#),
            ("awaiting_input", r#"{"prompt":"?"}"#),
            ("human_input", r#"{"text":"ok"}"#),
            (
                "critic_verdict",
                r#"{"reviewer":"r","kind":"fresh","approved":true}"#,
            ),
            ("loop_guard", r#"{"guard":"g","action":"stop"}"#),
        ] {
            assert!(
                !dispatch(&ev(event.0, event.1), &mut session),
                "{} must not end the turn",
                event.0
            );
        }
    }

    /// An undecodable event is surfaced as raw data (so a protocol drift is visible rather than
    /// silently swallowed) and does not end the turn.
    #[test]
    fn undecodable_events_print_raw_and_continue() {
        let mut session = None;
        assert!(!dispatch(&ev("tool_started", "not-json"), &mut session));
    }

    /// An unknown event *type* decodes as an empty token — forward-compatible, and never fatal.
    #[test]
    fn unknown_event_types_are_ignored() {
        let mut session = None;
        assert!(!dispatch(&ev("something_future", "whatever"), &mut session));
    }
}
