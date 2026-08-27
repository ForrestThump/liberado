//! Append-and-flush Model View Log (and optional execution log) at the request boundary.
//!
//! This is the production emitter for backlog **0.6**. Events are written as complete JSONL
//! lines and flushed (`sync_all`) before the function returns. Nothing is buffered until
//! process exit. The writer does not convert a finished `CoderEvent` document.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use liberado_provider::{
    CompletionRequest, CompletionResponse, FinishReason, Message, Role, ToolDef, ToolInvocation,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// One JSONL stream: append a complete object, then flush to durable storage.
struct JsonlWriter {
    file: Mutex<File>,
    run: String,
    seq: AtomicI64,
}

impl JsonlWriter {
    /// Create a fresh log, truncating any pre-existing content at `path`.
    ///
    /// `open` always means "start a new session". Appending to a stale file would let a
    /// reused path (e.g. a temp dir keyed only by pid, or a retried session id) inherit
    /// another run's events, which the seq-gap oracle then rejects. Per-event durability is
    /// still append-and-flush: each `emit` writes to the current end of the (now fresh) file.
    fn create(path: &Path, run: impl Into<String>) -> io::Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
            run: run.into(),
            seq: AtomicI64::new(0),
        })
    }

    fn emit(&self, type_name: &str, extra: Value) -> io::Result<()> {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let mut obj = match extra {
            Value::Object(map) => map,
            other => {
                let mut map = serde_json::Map::new();
                map.insert("body".into(), other);
                map
            }
        };
        obj.insert("v".into(), json!(1));
        obj.insert("type".into(), json!(type_name));
        obj.insert("ts".into(), json!(rfc3339_now()));
        obj.insert("run".into(), json!(&self.run));
        obj.insert("seq".into(), json!(seq));
        let line = serde_json::to_string(&Value::Object(obj))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("mvl writer lock poisoned"))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), canonical_value(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

fn sha256_json(value: &Value) -> String {
    let encoded = serde_json::to_string(&canonical_value(value)).unwrap_or_default();
    sha256_hex(encoded.as_bytes())
}

fn message_item(message: &Message) -> Value {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let mut item = serde_json::Map::new();
    item.insert("role".into(), json!(role));
    item.insert("content".into(), json!(message.content));
    if !message.tool_calls.is_empty() {
        item.insert(
            "tool_calls".into(),
            json!(
                message
                    .tool_calls
                    .iter()
                    .map(|c| json!({
                        "id": c.id,
                        "name": c.name,
                        "arguments": c.arguments,
                    }))
                    .collect::<Vec<_>>()
            ),
        );
    }
    if let Some(id) = &message.tool_call_id {
        item.insert("tool_call_id".into(), json!(id));
    }
    Value::Object(item)
}

fn catalog_definitions(tools: &[ToolDef]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.parameters,
                })
            })
            .collect(),
    )
}

struct SessionState {
    catalogs: HashSet<String>,
    systems: HashSet<String>,
    last_offered: Option<BTreeSet<String>>,
    last_message_len: usize,
    prompt_seen: bool,
}

/// Production MVL (+ optional execution) session. Attach to [`crate::Executor`] with
/// [`crate::Executor::with_mvl`].
pub struct MvlSession {
    mvl_path: PathBuf,
    execution_path: Option<PathBuf>,
    mvl: JsonlWriter,
    execution: Option<JsonlWriter>,
    run_started: AtomicBool,
    state: Mutex<SessionState>,
}

impl MvlSession {
    /// Open an MVL file and, if `execution` is `Some`, a paired execution log. Same `run` id.
    pub fn open(mvl: &Path, execution: Option<&Path>, run: impl Into<String>) -> io::Result<Self> {
        let run = run.into();
        let execution_path = execution.map(Path::to_path_buf);
        let execution = match execution {
            Some(path) => Some(JsonlWriter::create(path, run.clone())?),
            None => None,
        };
        Ok(Self {
            mvl_path: mvl.to_path_buf(),
            execution_path,
            mvl: JsonlWriter::create(mvl, run)?,
            execution,
            run_started: AtomicBool::new(false),
            state: Mutex::new(SessionState {
                catalogs: HashSet::new(),
                systems: HashSet::new(),
                last_offered: None,
                last_message_len: 0,
                prompt_seen: false,
            }),
        })
    }

    pub fn mvl_path(&self) -> &Path {
        &self.mvl_path
    }

    pub fn execution_path(&self) -> Option<&Path> {
        self.execution_path.as_deref()
    }

    fn warn(err: io::Error, what: &str) {
        tracing::warn!(error = %err, "{what}");
    }

    /// `run_started` once per session. Safe to call before every request.
    pub fn start_run(&self, model_id: &str, provider: &str, task_text: Option<&str>) {
        if self
            .run_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let mut body = serde_json::Map::new();
        body.insert(
            "harness".into(),
            json!({"name": "liberado", "version": "0.1.0"}),
        );
        body.insert(
            "model".into(),
            json!({"id": model_id, "provider": provider}),
        );
        if let Some(text) = task_text {
            // Generic id: this emitter is used for every coding run, not one backlog item.
            body.insert("task".into(), json!({"id": "coding", "text": text}));
        }
        if let Err(e) = self.mvl.emit("run_started", Value::Object(body)) {
            Self::warn(e, "mvl run_started");
        }
        if let Some(ex) = &self.execution
            && let Err(e) = ex.emit("attempt_started", json!({"attempt": 0, "workspace": ""}))
        {
            Self::warn(e, "execution attempt_started");
        }
    }

    pub fn end_run(&self, outcome: &str, reason: &str) {
        if let Some(ex) = &self.execution
            && let Err(e) = ex.emit(
                "attempt_ended",
                json!({"attempt": 0, "outcome": outcome, "reason": reason}),
            )
        {
            Self::warn(e, "execution attempt_ended");
        }
        if let Err(e) = self.mvl.emit(
            "run_ended",
            json!({"outcome": outcome, "reason": reason, "gates": []}),
        ) {
            Self::warn(e, "mvl run_ended");
        }
    }

    /// Emit catalog / tools_changed / prompt for this request. Call **before** the provider.
    pub fn on_request(&self, turn: i64, request: &CompletionRequest) {
        let definitions = catalog_definitions(&request.tools);
        let catalog_sha = sha256_json(&definitions);
        let offered: BTreeSet<String> = request.tools.iter().map(|t| t.name.clone()).collect();
        let offered_list: Vec<String> = request.tools.iter().map(|t| t.name.clone()).collect();

        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };

        if state.catalogs.insert(catalog_sha.clone())
            && let Err(e) = self.mvl.emit(
                "tool_catalog",
                json!({"sha256": catalog_sha, "tools": definitions}),
            )
        {
            Self::warn(e, "mvl tool_catalog");
        }

        if let Some(prev) = &state.last_offered
            && *prev != offered
        {
            let removed: Vec<_> = prev.difference(&offered).cloned().collect();
            let added: Vec<_> = offered.difference(prev).cloned().collect();
            if let Err(e) = self.mvl.emit(
                "tools_changed",
                json!({
                    "turn": turn,
                    "removed": removed,
                    "added": added,
                    "reason": "offer",
                }),
            ) {
                Self::warn(e, "mvl tools_changed");
            }
        }
        state.last_offered = Some(offered);

        let system_text = request
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .map(|m| m.content.as_str())
            .unwrap_or("");
        let system_sha = sha256_hex(system_text.as_bytes());
        let system_full = if state.systems.insert(system_sha.clone()) {
            Value::String(system_text.to_string())
        } else {
            Value::Null
        };

        let mode = if state.prompt_seen { "delta" } else { "full" };
        let items: Vec<Value> = if mode == "full" {
            request.messages.iter().map(message_item).collect()
        } else {
            request
                .messages
                .iter()
                .skip(state.last_message_len)
                .map(message_item)
                .collect()
        };
        state.last_message_len = request.messages.len();
        state.prompt_seen = true;
        drop(state);

        let mut params = BTreeMap::new();
        if let Some(t) = request.temperature {
            params.insert("temperature", json!(t));
        }
        if let Some(n) = request.max_tokens {
            params.insert("max_tokens", json!(n));
        }

        if let Err(e) = self.mvl.emit(
            "prompt",
            json!({
                "turn": turn,
                "messages": {"mode": mode, "items": items},
                "system": {"sha256": system_sha, "text": system_full},
                "tool_catalog_sha256": catalog_sha,
                "tools_offered": offered_list,
                "params": params,
            }),
        ) {
            Self::warn(e, "mvl prompt");
        }
    }

    /// Emit the completion. Call after the provider returns.
    pub fn on_completion(&self, turn: i64, response: &CompletionResponse) {
        let tool_calls: Vec<Value> = response
            .tool_calls
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "name": c.name,
                    "arguments": c.arguments,
                })
            })
            .collect();
        let finish = match response.finish_reason {
            FinishReason::Stop => "stop",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::Length => "length",
            FinishReason::ContentFilter => "content_filter",
        };
        let usage = response.usage.map(|u| {
            json!({
                "input": u.prompt_tokens,
                "cached_input": u.cached_prompt_tokens.unwrap_or(0),
                "output": u.completion_tokens,
                "reasoning": u.reasoning_tokens.unwrap_or(0),
            })
        });
        if let Err(e) = self.mvl.emit(
            "completion",
            json!({
                "turn": turn,
                "text": response.content.clone().unwrap_or_default(),
                "tool_calls": tool_calls,
                "finish_reason": finish,
                "usage": usage,
            }),
        ) {
            Self::warn(e, "mvl completion");
        }
    }

    pub fn on_tool_started(&self, turn: i64, call: &ToolInvocation) {
        if let Some(ex) = &self.execution
            && let Err(e) = ex.emit(
                "tool_started",
                json!({
                    "turn": turn,
                    "call_id": call.id,
                    "name": call.name,
                }),
            )
        {
            Self::warn(e, "execution tool_started");
        }
    }

    /// `content_shown` must be the exact string pushed onto the model message list.
    pub fn on_tool_result(&self, turn: i64, call: &ToolInvocation, ok: bool, content_shown: &str) {
        if let Err(e) = self.mvl.emit(
            "tool_result",
            json!({
                "turn": turn,
                "call_id": call.id,
                "name": call.name,
                "ok": ok,
                "content_shown": content_shown,
                "truncated": false,
                "offloaded": false,
            }),
        ) {
            Self::warn(e, "mvl tool_result");
        }
        if let Some(ex) = &self.execution
            && let Err(e) = ex.emit(
                "tool_finished",
                json!({
                    "turn": turn,
                    "call_id": call.id,
                    "name": call.name,
                    "ok": ok,
                    "duration_ms": 0,
                    "bytes_out": content_shown.len(),
                }),
            )
        {
            Self::warn(e, "execution tool_finished");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "liberado-mvl-unit-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn emit_is_durable_before_return() {
        let path = scratch("mid.mvl.jsonl");
        let writer = JsonlWriter::create(&path, "r").unwrap();
        writer
            .emit("run_started", json!({"harness":{"name":"t"}}))
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("run_started"),
            "append-flush must persist before emit returns: {text}"
        );
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn create_truncates_stale_content_from_a_reused_path() {
        // A reused path (temp dir keyed by pid, or a retried session id) must not inherit a
        // previous run's events. Appending would leave two `run_started` records and a seq gap.
        let path = scratch("reuse.mvl.jsonl");
        std::fs::write(&path, "{\"type\":\"stale\"}\n").unwrap();
        let writer = JsonlWriter::create(&path, "r").unwrap();
        writer
            .emit("run_started", json!({"harness":{"name":"t"}}))
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("stale"),
            "open must start a fresh log, got stale content: {text}"
        );
        assert_eq!(
            text.lines().count(),
            1,
            "one event, not stale + new: {text}"
        );
    }

    #[test]
    fn on_request_writes_prompt_before_any_completion() {
        let path = scratch("req.mvl.jsonl");
        let session = MvlSession::open(&path, None, "run-req").unwrap();
        session.start_run("mock", "mock", Some("goal"));
        let request = CompletionRequest::new(vec![
            Message::system("You are the coder."),
            Message::user("do it"),
        ])
        .with_tools(vec![ToolDef::new(
            "search",
            "Search",
            json!({"type":"object"}),
        )]);
        session.on_request(0, &request);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"type\":\"prompt\""));
        assert!(text.contains("You are the coder."));
        assert!(!text.contains("\"type\":\"completion\""));
        assert!(
            text.contains("\"id\":\"coding\""),
            "run_started task id must be generic, got: {text}"
        );
        assert!(
            !text.contains("\"id\":\"0.6\""),
            "must not stamp a backlog number onto every coding run: {text}"
        );
    }

    #[test]
    fn on_completion_records_reported_reasoning_tokens() {
        let path = scratch("reason.mvl.jsonl");
        let session = MvlSession::open(&path, None, "run-r").unwrap();
        session.on_completion(
            0,
            &CompletionResponse {
                content: Some("ok".into()),
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Stop,
                usage: Some(liberado_provider::Usage {
                    prompt_tokens: 10,
                    completion_tokens: 4,
                    total_tokens: 14,
                    cached_prompt_tokens: None,
                    reasoning_tokens: Some(33),
                }),
            },
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"reasoning\":33"),
            "MVL must not hardcode reasoning 0 when the provider reported tokens: {text}"
        );
    }
}

#[cfg(test)]
#[path = "mvl_survivor_tests.rs"]
mod survivor_tests;
