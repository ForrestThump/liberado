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
#[path = "wire_tests.rs"]
mod tests;
