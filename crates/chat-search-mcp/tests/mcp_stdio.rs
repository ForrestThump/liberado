//! End-to-end stdio test: spawn the real `liberado-chat-search-mcp` binary and drive the MCP
//! protocol over stdin/stdout, the way the daemon's stdio transport would. This is the only way
//! to exercise `main()` (tracing init + `run_stdio`) and `ChatSearchServer::new()` (which reads
//! `sessions_dir()` from the env). Env is set per-child with `Command::env` — never on the test
//! process — so parallel execution stays race-free.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn fixture_jsonl() -> String {
    [
        r#"{"kind":"header","id":"01JVAAAAAAAAAAAAAAAAAAAAAA","title":"Warmup","parent_conversation":null,"spawned_by":null,"created_at":"2026-01-01T00:00:00Z"}"#,
        r#"{"kind":"node","id":"01JVAAAAAAAAAAAAAAAAAAAAB1","parent_id":null,"conversation_id":"01JVAAAAAAAAAAAAAAAAAAAAAA","author":"assistant","created_at":"2026-01-02T00:00:00Z","message":{"role":"assistant","content":"The user prefers dark mode.","tool_calls":[],"tool_call_id":null}}"#,
    ]
    .join("\n")
}

fn request(id: u64, method: &str, params: &str) -> String {
    match params.is_empty() {
        true => format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}"}}"#),
        false => format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#),
    }
}

#[test]
fn stdio_handshake_searches_the_converged_store() {
    let dir = tempfile::tempdir().unwrap();
    // `sessions_dir()` resolves to `<LIBERADO_DATA_DIR>/sessions`.
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("01JVAAAAAAAAAAAAAAAAAAAAAA.jsonl"),
        fixture_jsonl(),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_liberado-chat-search-mcp"))
        .env("LIBERADO_DATA_DIR", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("binary spawns");

    {
        // `take()` moves the handle out so dropping it closes the pipe — an `as_mut()` borrow
        // leaves the write end open and the child never sees EOF on stdin.
        let mut stdin = child.stdin.take().expect("stdin piped");
        writeln!(
            stdin,
            "{}",
            request(
                1,
                "initialize",
                r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0.0.1"}}"#
            )
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        writeln!(
            stdin,
            "{}",
            request(
                2,
                "tools/call",
                r#"{"name":"search_conversations","arguments":{"query":"dark mode","regex":false,"limit":10}}"#
            )
        )
        .unwrap();
    } // stdin dropped → EOF → server exits

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout piped")
        .read_to_string(&mut stdout)
        .unwrap();
    let status = child.wait().expect("wait");
    assert!(status.success(), "server must exit cleanly on EOF");

    let responses: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("response is JSON"))
        .collect();
    assert_eq!(
        responses.len(),
        2,
        "initialize + tools/call each get one response"
    );

    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"], "chat-search",
        "initialize response identifies the server"
    );
    assert_eq!(responses[0]["result"]["serverInfo"]["version"], "0.1.0");

    let text = responses[1]["result"]["content"][0]["text"]
        .as_str()
        .expect("tools/call returns text content");
    let v: serde_json::Value = serde_json::from_str(text).expect("content is the results JSON");
    assert_eq!(v["total_found"], 1);
    assert_eq!(v["matches"][0]["title"], "Warmup");
    assert!(
        v["matches"][0]["matches"][0]["content_snippet"]
            .as_str()
            .unwrap()
            .contains("dark mode")
    );
}

#[test]
fn stdio_initialize_with_unsupported_version_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sessions")).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_liberado-chat-search-mcp"))
        .env("LIBERADO_DATA_DIR", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("binary spawns");

    {
        let mut stdin = child.stdin.take().expect("stdin piped");
        writeln!(
            stdin,
            "{}",
            request(
                1,
                "initialize",
                r#"{"protocolVersion":"1999-01-01","capabilities":{},"clientInfo":{"name":"probe","version":"0.0.1"}}"#
            )
        )
        .unwrap();
    }

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout piped")
        .read_to_string(&mut stdout)
        .unwrap();
    let _ = child.wait();

    let responses: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("response is JSON"))
        .collect();
    assert_eq!(responses.len(), 1);
    assert!(
        responses[0]["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("protocol version"),
        "unsupported protocol version must be rejected: {}",
        responses[0]
    );
}
