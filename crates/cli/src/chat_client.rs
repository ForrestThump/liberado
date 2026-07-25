//! The `liberado chat` terminal client — a thin REPL over a running daemon server's chat API.
//!
//! This module embeds **no** agent logic: no `ChatSessions`, no provider, no store. It is purely an
//! HTTP/SSE client of `POST /api/chat/stream` (the shared client contract — see `docs/reference/api.md`),
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
