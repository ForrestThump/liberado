//! The binary speaks MCP on stdio and refuses a base URL the WebUI would not use.
//!
//! Env is set on the child only. The test process does not call `set_var`.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn request(id: u64, method: &str, params: &str) -> String {
    if params.is_empty() {
        format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}"}}"#)
    } else {
        format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#)
    }
}

fn spawn(extra_env: &[(&str, &str)]) -> std::process::Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_liberado-operator-dogfood-mcp"));
    command
        .env("LIBERADO_SERVER", "http://127.0.0.1:9")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.spawn().expect("binary spawns")
}

#[test]
fn stdio_lists_the_dogfood_tools() {
    let mut child = spawn(&[]);
    {
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
        writeln!(stdin, "{}", request(2, "tools/list", "{}")).unwrap();
    }

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout piped")
        .read_to_string(&mut stdout)
        .unwrap();
    let status = child.wait().expect("wait");
    assert!(
        status.success(),
        "server must exit cleanly on EOF: {stdout}"
    );

    let responses: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("response is JSON"))
        .collect();
    assert_eq!(responses.len(), 2, "{stdout}");
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "liberado-operator-dogfood-mcp"
    );
    let mut names: Vec<&str> = responses[1]["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "catalog",
            "continue_session",
            "create_agent",
            "get_history",
            "list_agents",
            "list_sessions",
            "read_replies",
            "send_human_message",
            "start_chat",
            "status",
            "workspace_smoke",
        ]
    );
    let list_sessions = responses[1]["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|tool| tool["name"] == "list_sessions")
        .expect("list_sessions is listed");
    let properties = list_sessions["inputSchema"]["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("list_sessions inputSchema.properties: {list_sessions}"));
    assert!(
        properties.contains_key("include_background"),
        "{list_sessions}"
    );
    assert!(properties.contains_key("limit"), "{list_sessions}");
}

#[test]
fn binary_rejects_a_non_http_base_from_the_env() {
    let mut child = spawn(&[("LIBERADO_SERVER", "file:///tmp/nope")]);
    drop(child.stdin.take());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr piped")
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().expect("wait");
    assert!(!status.success(), "a file URL must not start the server");
    assert!(
        stderr.contains("http or https") || stderr.contains("LIBERADO_SERVER"),
        "{stderr}"
    );
}
