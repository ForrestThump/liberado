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
                &format!("[tool] {name} {mark} {}\n", truncate(&result_preview, 200)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use chat_client_contract::native::SseEvent;
    use std::sync::{Arc, Mutex};

    fn sse(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
        }
    }

    fn dispatch(
        event: &SseEvent,
        daemon_session: &mut Option<String>,
    ) -> (bool, Vec<(String, Value)>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sink = calls.clone();
        let emit = move |name: &str, value: Value| {
            sink.lock().unwrap().push((name.to_string(), value));
            Ok::<_, String>(())
        };
        let finished = dispatch_sse(event, daemon_session, "acp-1", &emit).unwrap();
        (finished, calls.lock().unwrap().clone())
    }

    #[test]
    fn dispatch_session_sets_daemon_session() {
        let mut daemon_session = None;
        let (finished, calls) = dispatch(&sse("session", "sess-1"), &mut daemon_session);
        assert!(!finished);
        assert_eq!(daemon_session.as_deref(), Some("sess-1"));
        assert!(calls.is_empty());
    }

    #[test]
    fn dispatch_token_emits_a_text_chunk() {
        let mut daemon_session = Some("sess-1".into());
        let (finished, calls) = dispatch(&sse("token", "hello"), &mut daemon_session);
        assert!(!finished);
        assert_eq!(calls.len(), 1);
        let (name, value) = &calls[0];
        assert_eq!(name, "session/update");
        assert_eq!(value["update"]["sessionUpdate"], "agent_message_chunk");
        assert_eq!(value["update"]["content"]["type"], "text");
        assert_eq!(value["update"]["content"]["text"], "hello");
        assert_eq!(value["sessionId"], "acp-1");
    }

    #[test]
    fn dispatch_tool_started_formats_the_preview() {
        let mut daemon_session = None;
        let data = r#"{"name":"bash","args_preview":"ls -la"}"#;
        let (finished, calls) = dispatch(&sse("tool_started", data), &mut daemon_session);
        assert!(!finished);
        assert_eq!(calls.len(), 1);
        let text = calls[0].1["update"]["content"]["text"].as_str().unwrap();
        assert!(text.contains("[tool] bash(ls -la)"), "{text}");
    }

    #[test]
    fn dispatch_tool_finished_marks_ok_and_err() {
        let mut daemon_session = None;
        let (_, calls) = dispatch(
            &sse(
                "tool_finished",
                r#"{"name":"bash","ok":true,"result_preview":"fine"}"#,
            ),
            &mut daemon_session,
        );
        let text = calls[0].1["update"]["content"]["text"].as_str().unwrap();
        assert!(text.contains("[tool] bash ok fine"), "{text}");

        let mut daemon_session = None;
        let (_, calls) = dispatch(
            &sse(
                "tool_finished",
                r#"{"name":"bash","ok":false,"result_preview":"boom"}"#,
            ),
            &mut daemon_session,
        );
        let text = calls[0].1["update"]["content"]["text"].as_str().unwrap();
        assert!(text.contains("[tool] bash err boom"), "{text}");
    }

    #[test]
    fn dispatch_session_offered_emits_the_offer() {
        let mut daemon_session = None;
        let data = r#"{"id":"g1","domain":"coding","description":"fix the tests"}"#;
        let (finished, calls) = dispatch(&sse("session_offered", data), &mut daemon_session);
        assert!(!finished);
        let text = calls[0].1["update"]["content"]["text"].as_str().unwrap();
        assert!(text.contains("[offered coding session g1]"), "{text}");
        assert!(text.contains("fix the tests"), "{text}");
    }

    #[test]
    fn dispatch_session_finished_ends_the_turn() {
        let mut daemon_session = None;
        let (finished, calls) = dispatch(
            &sse("session_finished", r#"{"status":"ok","summary":"done"}"#),
            &mut daemon_session,
        );
        assert!(finished);
        assert!(calls.is_empty());
    }

    #[test]
    fn dispatch_failed_ends_the_turn_with_an_error_line() {
        let mut daemon_session = None;
        let (finished, calls) =
            dispatch(&sse("failed", r#"{"message":"boom"}"#), &mut daemon_session);
        assert!(finished);
        let text = calls[0].1["update"]["content"]["text"].as_str().unwrap();
        assert!(text.contains("[error] boom"), "{text}");
    }

    #[test]
    fn dispatch_unparseable_payload_is_non_fatal() {
        let mut daemon_session = None;
        let (finished, calls) = dispatch(
            &sse("tool_started", "this is not json"),
            &mut daemon_session,
        );
        assert!(!finished);
        assert!(calls.is_empty());
    }

    #[test]
    fn dispatch_unknown_event_kind_is_a_noop() {
        let mut daemon_session = None;
        let (finished, calls) = dispatch(&sse("no_such_event", "x"), &mut daemon_session);
        assert!(!finished);
        assert!(calls.is_empty());
    }
}
#[cfg(test)]
mod face_env_tests {
    use super::*;

    /// One lock for all three env tests: function-local statics would be three
    /// distinct mutexes guarding the same process-global variable — exclusion
    /// in name only.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Restores LIBERADO_SERVER on drop. Tests assert with `expect_err`, whose
    /// panic must not leave a foreign value behind for every later test in
    /// this binary.
    struct ServerEnvGuard {
        saved: Option<String>,
    }

    impl ServerEnvGuard {
        // SAFETY: callers hold ENV_LOCK across the guard's whole lifetime.
        fn capture() -> Self {
            Self {
                saved: std::env::var("LIBERADO_SERVER").ok(),
            }
        }
    }

    impl Drop for ServerEnvGuard {
        fn drop(&mut self) {
            // SAFETY: see capture; still under ENV_LOCK on every path.
            unsafe {
                match self.saved.take() {
                    Some(v) => std::env::set_var("LIBERADO_SERVER", v),
                    None => std::env::remove_var("LIBERADO_SERVER"),
                }
            }
        }
    }

    async fn lock_server_env() -> (tokio::sync::MutexGuard<'static, ()>, ServerEnvGuard) {
        let guard = ENV_LOCK.lock().await;
        let env = ServerEnvGuard::capture();
        (guard, env)
    }

    #[tokio::test]
    async fn server_base_prefers_the_env_over_the_default() {
        let _guard = lock_server_env().await;
        // SAFETY: under ENV_LOCK; ServerEnvGuard restores.
        unsafe { std::env::set_var("LIBERADO_SERVER", "http://127.0.0.1:9") };
        assert_eq!(server_base(), "http://127.0.0.1:9");
        unsafe { std::env::remove_var("LIBERADO_SERVER") };
        assert_eq!(server_base(), DEFAULT_SERVER);
    } // ServerEnvGuard drop restores the saved value.

    #[tokio::test]
    async fn an_unreachable_daemon_is_a_named_error_not_success() {
        let _guard = lock_server_env().await;
        // SAFETY: under ENV_LOCK; ServerEnvGuard restores even on panic.
        unsafe { std::env::set_var("LIBERADO_SERVER", "http://127.0.0.1:1") };
        let mut daemon_session = None;
        let err = run_face_turn(&mut daemon_session, "hi", "acp-1", &|_, _| Ok(()))
            .await
            .expect_err("nothing is listening there");
        assert!(err.contains("cannot reach daemon"), "{err}");
    }

    #[tokio::test]
    async fn a_non_200_daemon_response_surfaces_the_status() {
        let (_guard, mut env) = lock_server_env().await;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // SAFETY: under ENV_LOCK; env restores via guard on any exit path.
        unsafe {
            std::env::set_var("LIBERADO_SERVER", format!("http://127.0.0.1:{port}"));
        }
        env.saved = None; // we own the value now; nothing to restore.

        let server = std::thread::spawn(move || {
            use std::io::Read as _;
            let (mut sock, _) = listener.accept().unwrap();
            // Drain the request head before answering; answering mid-request
            // resets the client's send side and masquerades as a connect error.
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf);
            std::io::Write::write_all(
                &mut sock,
                b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        });

        let mut daemon_session = None;
        let err = run_face_turn(&mut daemon_session, "hi", "acp-1", &|_, _| Ok(()))
            .await
            .expect_err("a 503 must fail the turn");

        server.join().unwrap();
        assert!(err.contains("503"), "{err}");
    }
}
