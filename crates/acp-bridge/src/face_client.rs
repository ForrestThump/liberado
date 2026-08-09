//! Face / daemon chat path: thin HTTP+SSE client of `liberado serve`.
//!
//! Same wire as `liberado chat` (`POST /api/chat/stream`). The face agent,
//! vault tools, and `delegate` live **in the daemon** — this module only
//! streams events into ACP `session/update` notifications.

use chat_client_contract::native::{SseDecoder, SseEvent};
use chat_client_contract::{SessionEvent, SessionEventKind};
use futures::StreamExt;
use serde_json::{Value, json};

/// Default daemon base when `LIBERADO_SERVER` is unset.
pub const DEFAULT_SERVER: &str = "http://127.0.0.1:4201";

pub fn server_base() -> String {
    std::env::var("LIBERADO_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_string())
}

/// One face turn against the daemon. Updates `daemon_session` when the server assigns an id.
/// Emits ACP text/tool updates via `emit`.
pub async fn run_face_turn(
    daemon_session: &mut Option<String>,
    message: &str,
    acp_session_id: &str,
    // Send + Sync so the turn future can run on a spawned task (cancel interleaves on stdin).
    emit: &(dyn Fn(&str, Value) -> Result<(), String> + Send + Sync),
) -> Result<(), String> {
    let base = server_base();
    let endpoint = format!("{base}/api/chat/stream");
    let client = reqwest::Client::new();
    let body = json!({ "message": message, "session": daemon_session });

    let response = client
        .post(&endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            format!(
                "face mode: cannot reach daemon at {base} ({e}). \
                 Start it with: liberado serve <vault>  (or set LIBERADO_SERVER)"
            )
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("face mode: daemon returned {status}: {text}"));
    }

    let mut decoder = SseDecoder::default();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("face mode: stream error: {e}"))?;
        let text = String::from_utf8_lossy(&chunk);
        for event in decoder.push(&text) {
            if dispatch_sse(&event, daemon_session, acp_session_id, emit)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Returns true when the turn is finished.
fn dispatch_sse(
    event: &SseEvent,
    daemon_session: &mut Option<String>,
    acp_session_id: &str,
    emit: &(dyn Fn(&str, Value) -> Result<(), String> + Send + Sync),
) -> Result<bool, String> {
    let decoded = match SessionEvent::from_sse_data(&event.event, &event.data) {
        Ok(d) => d,
        Err(_) => {
            // Non-fatal: unknown event shape.
            return Ok(false);
        }
    };
    match decoded.kind {
        SessionEventKind::Session { id } => {
            *daemon_session = Some(id);
            Ok(false)
        }
        SessionEventKind::Token { text } => {
            emit_text(emit, acp_session_id, &text)?;
            Ok(false)
        }
        SessionEventKind::ToolStarted { name, args_preview } => {
            emit_text(
                emit,
                acp_session_id,
                &format!("\n[tool] {name}({})\n", truncate(&args_preview, 200)),
            )?;
            Ok(false)
        }
        SessionEventKind::ToolFinished {
            name,
            ok,
            result_preview,
        } => {
            let mark = if ok { "ok" } else { "err" };
            emit_text(
                emit,
                acp_session_id,
                &format!(
                    "[tool] {name} {mark} {}\n",
                    truncate(&result_preview, 200)
                ),
            )?;
            Ok(false)
        }
        SessionEventKind::SessionOffered {
            id,
            domain,
            description,
        } => {
            emit_text(
                emit,
                acp_session_id,
                &format!(
                    "\n[offered {domain} session {id}] {description}\n\
                     (join via liberado TUI/API — face mode streamed the offer)\n"
                ),
            )?;
            Ok(false)
        }
        SessionEventKind::SessionFinished { .. } => Ok(true),
        SessionEventKind::Failed { message } => {
            emit_text(emit, acp_session_id, &format!("\n[error] {message}\n"))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn emit_text(
    emit: &(dyn Fn(&str, Value) -> Result<(), String> + Send + Sync),
    session_id: &str,
    text: &str,
) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    emit(
        "session/update",
        json!({
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text }
            }
        }),
    )
}

fn truncate(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}


