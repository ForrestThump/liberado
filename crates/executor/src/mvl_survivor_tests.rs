//! Split from `mvl.rs`: kills the baseline campaign's survivors.
//!
//! Covers writer creation without a parent dir, timestamp and hash helpers,
//! message/catalog item rendering, the execution sidecar path, tools_changed
//! detection, full-vs-delta prompts, system-message hashing, run end events,
//! and tool start/result emission.

use super::*;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "liberado-mvl-survivor-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn lines_of(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn create_makes_missing_parent_directories() {
    let path = scratch("nested/deeper/session.mvl.jsonl");
    let result = MvlSession::open(&path, None, "run-create");
    assert!(result.is_ok(), "missing parents must be created");
    assert!(path.exists());
}

#[tokio::test]
async fn emitted_timestamps_are_rfc3339_with_millis() {
    let path = scratch("ts.mvl.jsonl");
    let session = MvlSession::open(&path, None, "run-ts").unwrap();
    session.start_run("model-x", "mock", Some("task text"));
    let lines = lines_of(&path);
    let ts = lines[0]["ts"].as_str().expect("ts is a string");
    let parsed = chrono::DateTime::parse_from_rfc3339(ts).expect("ts must parse as RFC 3339");
    assert!(
        ts.ends_with('Z') && ts.contains('.'),
        "millis + Z suffix expected: {ts}"
    );
    drop(parsed);
}

#[test]
fn canonical_value_sorts_object_keys_recursively() {
    let value = json!({"b": {"d": 1, "c": 2}, "a": [3, 4]});
    let canonical = canonical_value(&value);
    let rendered = canonical.to_string();
    assert_eq!(rendered, r#"{"a":[3,4],"b":{"c":2,"d":1}}"#, "{rendered}");
}

#[test]
fn sha256_json_is_key_order_independent_hex() {
    let a = sha256_json(&json!({"a": 1, "b": 2}));
    let b = sha256_json(&json!({"b": 2, "a": 1}));
    assert_eq!(a, b, "key order must not change the hash");
    assert_eq!(a.len(), 64, "sha256 hex digest length");
    assert!(
        a.chars().all(|c| c.is_ascii_hexdigit()),
        "digest must be hex: {a}"
    );
    assert_ne!(a, sha256_json(&json!({"a": 1})), "content matters");
}

#[test]
fn sha256_hex_matches_the_known_digest() {
    // Well-known SHA-256 of "abc".
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn message_items_carry_role_content_and_tool_calls() {
    let plain = Message {
        role: Role::User,
        content: "hi".into(),
        tool_calls: vec![],
        tool_call_id: None,
    };
    assert_eq!(
        message_item(&plain),
        json!({"role": "user", "content": "hi"})
    );

    let calling = Message {
        role: Role::Assistant,
        content: String::new(),
        tool_calls: vec![liberado_provider::ToolInvocation {
            id: "t1".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }],
        tool_call_id: None,
    };
    let item = message_item(&calling);
    assert_eq!(item["role"], "assistant");
    assert_eq!(item["tool_calls"][0]["name"], "read_file");
}

#[test]
fn catalog_definitions_render_name_description_and_schema() {
    let defs = vec![ToolDef {
        name: "read_file".into(),
        description: "read a file".into(),
        parameters: json!({"type": "object"}),
    }];
    assert_eq!(
        catalog_definitions(&defs),
        json!([{
            "name": "read_file",
            "description": "read a file",
            "input_schema": {"type": "object"}
        }])
    );
}

#[tokio::test]
async fn execution_sidecar_path_round_trips() {
    let mvl = scratch("side.mvl.jsonl");
    let exe = scratch("side.execution.jsonl");
    let session = MvlSession::open(&mvl, Some(&exe), "run-side").unwrap();
    assert_eq!(session.execution_path(), Some(exe.as_path()));

    let bare = MvlSession::open(&scratch("bare.mvl.jsonl"), None, "run-side").unwrap();
    assert_eq!(bare.execution_path(), None);
}

#[tokio::test]
async fn end_run_writes_the_terminal_events() {
    let mvl = scratch("end.mvl.jsonl");
    let exe = scratch("end.execution.jsonl");
    let session = MvlSession::open(&mvl, Some(&exe), "run-end").unwrap();
    session.end_run("succeeded", "report filed");

    let lines = lines_of(&mvl);
    let last = lines.last().unwrap();
    assert_eq!(last["type"], "run_ended", "{last}");
    assert_eq!(last["outcome"], "succeeded");
    assert_eq!(last["reason"], "report filed");

    let exe_lines = lines_of(&exe);
    let exe_last = exe_lines.last().unwrap();
    assert_eq!(exe_last["type"], "attempt_ended", "{exe_last}");
}

/// Identical offers must not emit tools_changed; a changed offer must, naming
/// what was added.
#[tokio::test]
async fn tools_changed_fires_only_on_real_changes() {
    let mvl = scratch("offer.mvl.jsonl");
    let session = MvlSession::open(&mvl, None, "run-offer").unwrap();
    let mk = |names: &[&str]| {
        let mut r = CompletionRequest::new(vec![Message {
            role: Role::User,
            content: "go".into(),
            tool_calls: vec![],
            tool_call_id: None,
        }]);
        r.tools = names
            .iter()
            .map(|n| ToolDef {
                name: (*n).into(),
                description: "d".into(),
                parameters: json!({}),
            })
            .collect();
        r
    };

    session.on_request(0, &mk(&["read_file"]));
    session.on_request(1, &mk(&["read_file"]));
    let count_same = lines_of(&mvl)
        .iter()
        .filter(|l| l["type"] == "tools_changed")
        .count();
    assert_eq!(count_same, 0, "identical offers are not a change");

    session.on_request(2, &mk(&["read_file", "write_file"]));
    let lines = lines_of(&mvl);
    let changed = lines
        .iter()
        .find(|l| l["type"] == "tools_changed")
        .expect("an added tool is a change");
    assert_eq!(changed["added"], json!(["write_file"]));
}

#[tokio::test]
async fn first_prompt_is_full_and_carries_the_system_hash() {
    let mvl = scratch("prompt.mvl.jsonl");
    let session = MvlSession::open(&mvl, None, "run-prompt").unwrap();
    let mut request = CompletionRequest::new(vec![
        Message {
            role: Role::System,
            content: "You are terse.".into(),
            tool_calls: vec![],
            tool_call_id: None,
        },
        Message {
            role: Role::User,
            content: "go".into(),
            tool_calls: vec![],
            tool_call_id: None,
        },
    ]);
    request.temperature = Some(0.5);
    session.on_request(0, &request);

    let prompt = lines_of(&mvl)
        .into_iter()
        .find(|l| l["type"] == "prompt")
        .expect("a prompt event");
    assert_eq!(prompt["messages"]["mode"], "full", "{prompt}");
    assert_eq!(
        prompt["messages"]["items"].as_array().unwrap().len(),
        2,
        "{prompt}"
    );
    assert_eq!(prompt["system"]["text"], "You are terse.", "{prompt}");
    assert_eq!(
        prompt["system"]["sha256"],
        json!(sha256_hex(b"You are terse.")),
        "{prompt}"
    );
    assert_eq!(prompt["params"]["temperature"], 0.5, "{prompt}");
}

#[tokio::test]
async fn tool_events_land_in_both_logs() {
    let mvl = scratch("tools.mvl.jsonl");
    let exe = scratch("tools.execution.jsonl");
    let session = MvlSession::open(&mvl, Some(&exe), "run-tools").unwrap();
    let call = liberado_provider::ToolInvocation {
        id: "call-7".into(),
        name: "read_file".into(),
        arguments: "{}".into(),
    };
    session.on_tool_started(3, &call);
    session.on_tool_result(3, &call, true, "file body");

    let started = lines_of(&exe)
        .into_iter()
        .find(|l| l["type"] == "tool_started")
        .expect("execution log records the start");
    assert_eq!(started["call_id"], "call-7");
    assert_eq!(started["name"], "read_file");

    let result = lines_of(&mvl)
        .into_iter()
        .find(|l| l["type"] == "tool_result")
        .expect("mvl records the result");
    assert_eq!(result["ok"], true);
    assert_eq!(result["content_shown"], "file body");
}
