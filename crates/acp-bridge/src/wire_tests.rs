//! Split from `wire.rs` for module-health boundaries.

use super::*;

struct RecordingSink {
    lines: std::sync::Mutex<Vec<(String, Value)>>,
}

impl WireSink for RecordingSink {
    fn emit(&self, method: &str, params: Value) -> Result<(), String> {
        self.lines
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(())
    }

    fn write_rpc_response(
        &self,
        id: Value,
        outcome: Result<Value, JsonRpcErrorBody>,
    ) -> Result<(), String> {
        let body = match outcome {
            Ok(result) => json!({ "id": id, "result": result }),
            Err(error) => json!({ "id": id, "error": error.code }),
        };
        self.lines
            .lock()
            .unwrap()
            .push(("response".to_string(), body));
        Ok(())
    }
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            lines: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<(String, Value)> {
        self.lines.lock().unwrap().clone()
    }
}

#[test]
fn tool_kind_maps_every_offered_tool() {
    assert_eq!(tool_kind("read_file"), "read");
    assert_eq!(tool_kind("list_files"), "read");
    assert_eq!(tool_kind("search_text"), "read");
    assert_eq!(tool_kind("write_file"), "edit");
    assert_eq!(tool_kind("edit_file"), "edit");
    assert_eq!(tool_kind("apply_patch"), "edit");
    assert_eq!(tool_kind("run_command"), "execute");
    assert_eq!(tool_kind("validate"), "execute");
    assert_eq!(tool_kind("git_status"), "execute");
    assert_eq!(tool_kind("git_diff"), "execute");
    assert_eq!(tool_kind("git_branch"), "execute");
    assert_eq!(tool_kind("git_commit"), "execute");
    assert_eq!(tool_kind("git_push"), "execute");
    assert_eq!(tool_kind("git_fetch"), "execute");
    assert_eq!(tool_kind("ask_human"), "other");
    assert_eq!(tool_kind("totally_new_tool"), "other");
}

#[test]
fn pop_with_no_start_mints_an_orphan_id() {
    let mut pending = Vec::new();
    let orphan = pop_tool_call_id(&mut pending, "run_command");
    assert!(
        orphan.starts_with("call-orphan-"),
        "a finish with no start must still emit a valid id: {orphan}"
    );
    assert_ne!(pop_tool_call_id(&mut pending, "x"), orphan);
}

#[test]
fn empty_text_chunks_emit_nothing() {
    let sink = RecordingSink::new();
    emit_agent_text_chunk(&sink, "s1", "").unwrap();
    emit_user_message_chunk(&sink, "s1", "").unwrap();
    assert!(
        sink.calls().is_empty(),
        "empty text must not reach the wire"
    );
}

#[test]
fn text_chunks_carry_their_session_update_kind() {
    let sink = RecordingSink::new();
    emit_agent_text_chunk(&sink, "s1", "hi").unwrap();
    emit_user_message_chunk(&sink, "s2", "yo").unwrap();
    let calls = sink.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "session/update");
    assert_eq!(calls[0].1["update"]["sessionUpdate"], "agent_message_chunk");
    assert_eq!(calls[0].1["sessionId"], "s1");
    assert_eq!(calls[1].1["update"]["sessionUpdate"], "user_message_chunk");
    assert_eq!(calls[1].1["update"]["content"]["text"], "yo");
}

#[test]
fn tool_call_parses_json_input_and_falls_back_to_raw() {
    let sink = RecordingSink::new();
    emit_tool_call(
        &sink,
        "s1",
        "call-1",
        "read_file",
        r#"{"path":"a.rs"}"#,
        "pending",
    )
    .unwrap();
    emit_tool_call(&sink, "s1", "call-2", "run_command", "not json", "pending").unwrap();
    let calls = sink.calls();
    assert_eq!(calls[0].1["update"]["rawInput"]["path"], "a.rs");
    assert_eq!(calls[0].1["update"]["kind"], "read");
    assert_eq!(calls[1].1["update"]["rawInput"]["raw"], "not json");
    assert_eq!(calls[1].1["update"]["toolCallId"], "call-2");
}

#[test]
fn tool_call_update_carries_status_and_preview() {
    let sink = RecordingSink::new();
    emit_tool_call_update(&sink, "s1", "call-9", "validate", "completed", "all green").unwrap();
    let (method, value) = &sink.calls()[0];
    assert_eq!(method, "session/update");
    assert_eq!(value["update"]["sessionUpdate"], "tool_call_update");
    assert_eq!(value["update"]["status"], "completed");
    assert_eq!(
        value["update"]["content"][0]["content"]["text"],
        "all green"
    );
}

#[test]
fn short_id_is_hex_and_unique_enough_for_pairing() {
    let a = short_id();
    let b = short_id();
    assert!(!a.is_empty());
    assert!(
        a.chars().all(|c| c.is_ascii_hexdigit()),
        "ids are hex nanos: {a}"
    );
    assert_ne!(a, b);
}

#[test]
fn responses_omit_the_absent_half_of_result_or_error() {
    let ok = JsonRpcResponse {
        jsonrpc: "2.0",
        id: json!(7),
        result: Some(json!({ "stopReason": "end_turn" })),
        error: None,
    };
    let ok_json = serde_json::to_string(&ok).unwrap();
    assert!(ok_json.contains("\"result\""));
    assert!(!ok_json.contains("\"error\""));

    let err = JsonRpcResponse {
        jsonrpc: "2.0",
        id: json!("lib-perm-1"),
        result: None,
        error: Some(JsonRpcErrorBody {
            code: -32601,
            message: "Method not found: x".into(),
        }),
    };
    let err_json = serde_json::to_string(&err).unwrap();
    assert!(!err_json.contains("\"result\""));
    assert!(err_json.contains("-32601"));
}
