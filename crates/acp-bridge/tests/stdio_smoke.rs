//! Stdio smoke: spawn `liberado-acp`, drive ACP initialize + session/new without an API key.
//!
//! Codifies the install-script probe so CI catches wire regressions. No network.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

/// Path to the package binary. Cargo injects this at **test runtime** as
/// `CARGO_BIN_EXE_liberado-acp` (hyphen preserved in current Cargo).
fn bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_liberado-acp", "CARGO_BIN_EXE_liberado_acp"] {
        if let Ok(p) = std::env::var(key) {
            return PathBuf::from(p);
        }
    }
    panic!("CARGO_BIN_EXE_liberado-acp not set; run via `cargo test -p liberado-acp-bridge`");
}

fn read_json_line(reader: &mut impl BufRead) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read ACP response line");
    assert!(
        !line.trim().is_empty(),
        "expected a JSON-RPC response line on stdout"
    );
    serde_json::from_str(line.trim()).unwrap_or_else(|e| {
        panic!("response is not JSON: {e}; line={line:?}");
    })
}

#[test]
fn initialize_and_session_new_over_stdio() {
    let bin = bin();
    assert!(
        bin.is_file(),
        "liberado-acp binary missing at {}",
        bin.display()
    );

    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("LIBERADO_CONFIG_DIR")
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    // ── initialize ──────────────────────────────────────────────────
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientInfo": { "name": "stdio-smoke", "version": "0" },
            "clientCapabilities": {}
        }
    });
    writeln!(stdin, "{init}").expect("write initialize");
    stdin.flush().expect("flush initialize");

    let init_resp = read_json_line(&mut reader);
    assert_eq!(init_resp["jsonrpc"], "2.0");
    assert_eq!(init_resp["id"], 1);
    assert!(
        init_resp.get("error").is_none() || init_resp["error"].is_null(),
        "initialize error: {init_resp}"
    );
    let result = &init_resp["result"];
    assert_eq!(result["protocolVersion"], 1);
    assert_eq!(result["agentInfo"]["name"], "Liberado");
    assert_eq!(
        result["agentCapabilities"]["loadSession"], false,
        "must not advertise loadSession without durable history"
    );

    // ── session/new ─────────────────────────────────────────────────
    let cwd = std::env::temp_dir();
    let new_session = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {
            "cwd": cwd,
            "mcpServers": []
        }
    });
    writeln!(stdin, "{new_session}").expect("write session/new");
    stdin.flush().expect("flush session/new");

    let new_resp = read_json_line(&mut reader);
    assert_eq!(new_resp["id"], 2);
    assert!(
        new_resp.get("error").is_none() || new_resp["error"].is_null(),
        "session/new error: {new_resp}"
    );
    let session = &new_resp["result"];
    let sid = session["sessionId"].as_str().expect("sessionId string");
    assert!(!sid.is_empty(), "sessionId must be non-empty");
    assert!(
        session["models"]["currentModelId"].is_string(),
        "models.currentModelId required: {session}"
    );
    assert_eq!(
        session["modes"]["currentModeId"], "coding",
        "default mode is Liberado coding pack"
    );
    let modes = session["modes"]["availableModes"]
        .as_array()
        .expect("availableModes");
    assert_eq!(modes.len(), 3, "coding · chat · face: {modes:?}");
    let mode_ids: Vec<&str> = modes.iter().filter_map(|m| m["id"].as_str()).collect();
    assert!(mode_ids.contains(&"coding"));
    assert!(mode_ids.contains(&"chat"));
    assert!(mode_ids.contains(&"face"));

    // ── session/set_mode → chat ─────────────────────────────────────
    let set_mode = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/set_mode",
        "params": {
            "sessionId": sid,
            "modeId": "chat"
        }
    });
    writeln!(stdin, "{set_mode}").expect("write session/set_mode");
    stdin.flush().expect("flush session/set_mode");

    let mode_resp = read_json_line(&mut reader);
    assert_eq!(mode_resp["id"], 3);
    assert!(
        mode_resp.get("error").is_none() || mode_resp["error"].is_null(),
        "session/set_mode error: {mode_resp}"
    );

    // Close stdin so the agent exits cleanly.
    drop(stdin);

    let status = wait_with_timeout(&mut child, Duration::from_secs(15));
    assert!(
        status.success(),
        "liberado-acp should exit 0 after stdin close; got {status}"
    );
}

/// Avoid hanging the suite if the binary never exits.
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::process::ExitStatus {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("liberado-acp did not exit within {timeout:?}");
            }
            Err(e) => panic!("wait failed: {e}"),
        }
    }
}
