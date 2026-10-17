//! Split from `face_client.rs` for module-health boundaries.
//!
//! Covers the `run_face_turn` branches that read a live SSE body: a finished
//! stream, a stream that ends without `session_finished`, and an emit failure.
//! Shares `face_env_tests`'s env lock so these tests and the ones still in
//! `face_client.rs` cannot clobber `LIBERADO_SERVER` at the same time.

use super::face_env_tests::lock_server_env;
use std::sync::{Arc, Mutex};

fn spawn_http(status_and_body: String) -> (u16, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 2048];
        let _ = sock.read(&mut buf);
        sock.write_all(status_and_body.as_bytes()).unwrap();
    });
    (port, server)
}

fn sse_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn a_finished_sse_stream_emits_text_and_ends_the_turn() {
    let (_guard, _env) = lock_server_env().await;
    let body = "\
event: session\n\
data: sess-9\n\
\n\
event: token\n\
data: hello-face\n\
\n\
event: session_finished\n\
data: {\"status\":\"ok\",\"summary\":\"done\"}\n\
\n";
    let (port, server) = spawn_http(sse_response(body));
    // SAFETY: under ENV_LOCK; ServerEnvGuard restores the previous value.
    unsafe {
        std::env::set_var("LIBERADO_SERVER", format!("http://127.0.0.1:{port}"));
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sink = calls.clone();
    let mut daemon_session = None;
    let result = super::run_face_turn(&mut daemon_session, "hi", "acp-1", &move |name, value| {
        sink.lock().unwrap().push((name.to_string(), value));
        Ok(())
    })
    .await;
    server.join().unwrap();
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(daemon_session.as_deref(), Some("sess-9"));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].1["update"]["content"]["text"], "hello-face");
}

#[tokio::test]
async fn a_stream_that_ends_without_session_finished_is_still_success() {
    let (_guard, _env) = lock_server_env().await;
    let (port, server) = spawn_http(sse_response("event: token\ndata: partial\n\n"));
    // SAFETY: under ENV_LOCK; ServerEnvGuard restores the previous value.
    unsafe {
        std::env::set_var("LIBERADO_SERVER", format!("http://127.0.0.1:{port}"));
    }
    let mut daemon_session = None;
    let result = super::run_face_turn(&mut daemon_session, "hi", "acp-1", &|_, _| Ok(())).await;
    server.join().unwrap();
    assert!(result.is_ok(), "{result:?}");
    assert!(daemon_session.is_none());
}

#[tokio::test]
async fn an_emit_failure_fails_the_turn() {
    let (_guard, _env) = lock_server_env().await;
    let (port, server) = spawn_http(sse_response("event: token\ndata: hello-face\n\n"));
    // SAFETY: under ENV_LOCK; ServerEnvGuard restores the previous value.
    unsafe {
        std::env::set_var("LIBERADO_SERVER", format!("http://127.0.0.1:{port}"));
    }
    let mut daemon_session = None;
    let err = super::run_face_turn(&mut daemon_session, "hi", "acp-1", &|_, _| {
        Err("emit failed".to_string())
    })
    .await
    .expect_err("a failing emit must fail the turn");
    server.join().unwrap();
    assert!(err.contains("emit failed"), "{err}");
}
