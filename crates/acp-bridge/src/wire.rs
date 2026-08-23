//! The ACP wire: JSON-RPC framing and the `session/update` notifications Paseo renders.
//!
//! Split out of `main.rs`. This module knows the protocol and nothing else — no sessions, no
//! provider, no modes — so a change to what Paseo renders is a change in one file, and the
//! tool-call id pairing that the UI depends on sits next to the code that emits it.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ── JSON-RPC framing ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct JsonRpcIncoming {
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) jsonrpc: Option<String>,
    pub(crate) id: Option<Value>,
    pub(crate) method: Option<String>,
    #[serde(default)]
    pub(crate) params: Value,
    /// Present on JSON-RPC *responses* (client answering an agent request).
    #[serde(default)]
    pub(crate) result: Option<Value>,
    #[serde(default)]
    pub(crate) error: Option<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcResponse {
    pub(crate) jsonrpc: &'static str,
    pub(crate) id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<JsonRpcErrorBody>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcErrorBody {
    pub(crate) code: i32,
    pub(crate) message: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcNotification {
    pub(crate) jsonrpc: &'static str,
    pub(crate) method: String,
    pub(crate) params: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcRequest {
    pub(crate) jsonrpc: &'static str,
    pub(crate) id: Value,
    pub(crate) method: String,
    pub(crate) params: Value,
}

pub(crate) trait WireSink: Send + Sync {
    fn emit(&self, method: &str, params: Value) -> Result<(), String>;
    /// Serialize and deliver one JSON-RPC response carrying a result or an error body.
    fn write_rpc_response(
        &self,
        id: Value,
        outcome: Result<Value, JsonRpcErrorBody>,
    ) -> Result<(), String>;
}

/// Unified stdout writer for responses and notifications (one lock, NDJSON-safe).
pub(crate) struct StdoutWire;

impl StdoutWire {
    pub(crate) fn write_line(&self, json: &str) -> Result<(), String> {
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        writeln!(out, "{json}").map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Agent → client request (permission). Ids are strings so they cannot collide with
    /// Paseo's numeric client ids. Digit-only strings are coerced to numbers by Paseo.
    pub(crate) fn write_rpc_request(
        &self,
        id: Value,
        method: &str,
        params: Value,
    ) -> Result<(), String> {
        let body = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };
        let json = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        self.write_line(&json)
    }
}

impl WireSink for StdoutWire {
    fn emit(&self, method: &str, params: Value) -> Result<(), String> {
        let body = JsonRpcNotification {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        };
        let json = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        self.write_line(&json)
    }

    fn write_rpc_response(
        &self,
        id: Value,
        outcome: Result<Value, JsonRpcErrorBody>,
    ) -> Result<(), String> {
        let body = match outcome {
            Ok(result) => JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(result),
                error: None,
            },
            Err(error) => JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(error),
            },
        };
        let json = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        self.write_line(&json)
    }
}

pub(crate) fn push_tool_call_id(pending: &mut Vec<(String, String)>, name: &str) -> String {
    let tool_call_id = format!("call-{}", short_id());
    pending.push((name.to_string(), tool_call_id.clone()));
    tool_call_id
}

/// Pop the most recent in-flight id for `name` (LIFO). Fallback id only if start was missed.
pub(crate) fn pop_tool_call_id(pending: &mut Vec<(String, String)>, name: &str) -> String {
    if let Some(idx) = pending.iter().rposition(|(n, _)| n == name) {
        return pending.remove(idx).1;
    }
    // Should not happen in a well-formed stream; still emit a unique id so the wire is valid.
    format!("call-orphan-{}", short_id())
}

pub(crate) fn emit_agent_text_chunk(
    sink: &dyn WireSink,
    session_id: &str,
    text: &str,
) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    sink.emit(
        "session/update",
        json!({
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {
                    "type": "text",
                    "text": text
                }
            }
        }),
    )
}

pub(crate) fn emit_user_message_chunk(
    sink: &dyn WireSink,
    session_id: &str,
    text: &str,
) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    sink.emit(
        "session/update",
        json!({
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "user_message_chunk",
                "content": {
                    "type": "text",
                    "text": text
                }
            }
        }),
    )
}

pub(crate) fn emit_tool_call(
    sink: &dyn WireSink,
    session_id: &str,
    tool_call_id: &str,
    name: &str,
    args: &str,
    status: &str,
) -> Result<(), String> {
    let raw_input: Value = serde_json::from_str(args).unwrap_or_else(|_| json!({ "raw": args }));
    sink.emit(
        "session/update",
        json!({
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": tool_call_id,
                "title": name,
                "kind": tool_kind(name),
                "status": status,
                "rawInput": raw_input
            }
        }),
    )
}

pub(crate) fn emit_tool_call_update(
    sink: &dyn WireSink,
    session_id: &str,
    tool_call_id: &str,
    name: &str,
    status: &str,
    preview: &str,
) -> Result<(), String> {
    sink.emit(
        "session/update",
        json!({
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": tool_call_id,
                "status": status,
                "title": name,
                "content": [{
                    "type": "content",
                    "content": { "type": "text", "text": preview }
                }]
            }
        }),
    )
}

pub(crate) fn tool_kind(name: &str) -> &'static str {
    match name {
        "read_file" | "list_files" | "search_text" => "read",
        "write_file" | "edit_file" | "apply_patch" => "edit",
        "run_command" | "validate" => "execute",
        "git_status" | "git_diff" | "git_branch" | "git_commit" | "git_push" | "git_fetch" => {
            "execute"
        }
        _ => "other",
    }
}

pub(crate) fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}")
}

#[cfg(test)]
mod tests {
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
}
