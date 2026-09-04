//! OpenCode worker adapter communicating via Agent Client Protocol (ACP) over stdio.
//!
//! OpenCode runs as an external coding worker speaking ACP over stdio JSON-RPC.
//! This adapter manages session handshakes, model configuration (e.g. OpenRouter DeepSeek),
//! permission auto-approvals, tool turn collection, and git artifact inspection.

use super::{
    ControlPlaneError, RunHandle, TaskEvent, WorkerPort, WorkerRunRequest, WorkerRunResult,
    WorkerStatus,
};
use chrono::Utc;
use liberado_common::process::std_command;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::{Arc, RwLock};

/// Configuration for the OpenCode external worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenCodeWorkerConfig {
    pub executable: Option<String>,
    pub model: String,
    pub auto_approve: bool,
}

impl Default for OpenCodeWorkerConfig {
    fn default() -> Self {
        Self {
            executable: None,
            model: "openrouter/~deepseek/deepseek-v4-flash-latest".into(),
            auto_approve: true,
        }
    }
}

/// Worker implementation driving OpenCode via ACP JSON-RPC.
pub struct OpenCodeWorker {
    config: OpenCodeWorkerConfig,
    results: Arc<RwLock<HashMap<String, WorkerRunResult>>>,
}

impl OpenCodeWorker {
    pub fn new(config: OpenCodeWorkerConfig) -> Self {
        Self {
            config,
            results: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn config(&self) -> &OpenCodeWorkerConfig {
        &self.config
    }

    /// Spawns the ACP server process.
    fn spawn_acp_process(&self, worktree: &str) -> Result<std::process::Child, ControlPlaneError> {
        let mut cmd = if cfg!(windows) {
            let mut c = std_command("cmd");
            c.args(["/c", "opencode", "acp"]);
            c
        } else {
            let exe = self.config.executable.as_deref().unwrap_or("opencode");
            let mut c = std_command(exe);
            c.arg("acp");
            c
        };

        cmd.current_dir(worktree);
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        cmd.spawn().map_err(ControlPlaneError::Io)
    }

    /// Execute a full ACP turn in the designated worktree.
    pub fn execute_turn(
        &self,
        worktree: &str,
        prompt: &str,
        resumed_session_id: Option<&str>,
    ) -> Result<(WorkerRunResult, String), ControlPlaneError> {
        let mut child = self.spawn_acp_process(worktree)?;
        let (mut stdin, mut reader) = take_stdio(&mut child)?;

        let (session_id, assistant_summary, stop_reason) = run_acp_session(
            &mut stdin,
            &mut reader,
            worktree,
            &self.config.model,
            resumed_session_id,
            prompt,
            self.config.auto_approve,
        )?;

        drop(stdin);
        let _ = child.kill();

        let (commits, files_changed) = inspect_git_worktree(worktree);
        let result = build_worker_result(
            &stop_reason,
            &assistant_summary,
            commits,
            files_changed,
            session_id.clone(),
        );

        Ok((result, session_id))
    }
}

impl WorkerPort for OpenCodeWorker {
    fn id(&self) -> &str {
        "opencode"
    }

    fn start(&self, req: &WorkerRunRequest) -> Result<RunHandle, ControlPlaneError> {
        let run_id = format!("run-{}", Utc::now().timestamp_millis());
        let (result, session_id) =
            self.execute_turn(&req.worktree, &req.prompt, req.session_id.as_deref())?;

        let handle = RunHandle::new(run_id.clone(), self.id(), req.task_id.clone())
            .with_session_id(session_id)
            .with_worktree(req.worktree.clone());

        if let Ok(mut lock) = self.results.write() {
            lock.insert(run_id, result);
        }

        Ok(handle)
    }

    fn resume(
        &self,
        handle: &RunHandle,
        event: &TaskEvent,
    ) -> Result<RunHandle, ControlPlaneError> {
        let resume_prompt = synthesize_resume_prompt(event);
        let run_id = format!("run-{}", Utc::now().timestamp_millis());
        let worktree = handle.worktree.as_deref().unwrap_or(".");
        let (result, session_id) = self.execute_turn(
            worktree,
            &resume_prompt,
            handle.external_session_id.as_deref(),
        )?;

        let mut new_handle = RunHandle::new(run_id.clone(), self.id(), handle.task_id.clone())
            .with_session_id(session_id);
        if let Some(wt) = &handle.worktree {
            new_handle = new_handle.with_worktree(wt.clone());
        }

        if let Ok(mut lock) = self.results.write() {
            lock.insert(run_id, result);
        }

        Ok(new_handle)
    }

    fn status(&self, handle: &RunHandle) -> Result<WorkerStatus, ControlPlaneError> {
        let lock = self
            .results
            .read()
            .map_err(|_| ControlPlaneError::EmptyHistory)?;
        if let Some(res) = lock.get(&handle.run_id) {
            Ok(res.status)
        } else {
            Ok(WorkerStatus::Waiting)
        }
    }

    fn cancel(&self, handle: &RunHandle) -> Result<(), ControlPlaneError> {
        if let Ok(mut lock) = self.results.write() {
            lock.remove(&handle.run_id);
        }
        Ok(())
    }

    fn collect(&self, handle: &RunHandle) -> Result<WorkerRunResult, ControlPlaneError> {
        let mut lock = self
            .results
            .write()
            .map_err(|_| ControlPlaneError::EmptyHistory)?;
        lock.remove(&handle.run_id).ok_or_else(|| {
            ControlPlaneError::InvalidInitialEvent(format!("no result for run {}", handle.run_id))
        })
    }
}

pub(crate) fn synthesize_resume_prompt(event: &TaskEvent) -> String {
    match &event.payload {
        super::TaskEventKind::CiFailed {
            failures,
            failure_log_excerpt,
            ..
        } => {
            let mut p = String::from(
                "CI failed on your pull request. Please fix the following failures:\n",
            );
            for f in failures {
                p.push_str(&format!("- {f}\n"));
            }
            if let Some(excerpt) = failure_log_excerpt {
                p.push_str("\nFailure log excerpt:\n```text\n");
                p.push_str(excerpt);
                p.push_str("\n```\n");
            }
            p.push_str("\nReproduce locally, fix the issues, and do not broaden scope.\n");
            p
        }
        super::TaskEventKind::ReviewRejected { diagnosis, .. } => {
            format!(
                "Review was rejected with the following diagnosis:\n{diagnosis}\n\nPlease address the review feedback.\n"
            )
        }
        _ => "Continue working on the task.".to_string(),
    }
}

pub(crate) fn build_worker_result(
    stop_reason: &str,
    assistant_summary: &str,
    commits: Vec<String>,
    files_changed: Vec<String>,
    session_id: String,
) -> WorkerRunResult {
    let is_error = stop_reason.contains("error");
    let status = if is_error {
        WorkerStatus::Failed
    } else {
        WorkerStatus::Completed
    };

    WorkerRunResult {
        status,
        summary: assistant_summary.trim().to_string(),
        commits,
        files_changed,
        tests_run: 0,
        tests_passed: 0,
        blocking_issue: if is_error {
            Some(stop_reason.to_string())
        } else {
            None
        },
        recommended_next_action: None,
        external_session_id: Some(session_id),
    }
}

pub(crate) fn take_stdio(
    child: &mut std::process::Child,
) -> Result<
    (
        std::process::ChildStdin,
        BufReader<std::process::ChildStdout>,
    ),
    ControlPlaneError,
> {
    let stdin = child.stdin.take().ok_or_else(|| {
        ControlPlaneError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "child stdin unavailable",
        ))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ControlPlaneError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "child stdout unavailable",
        ))
    })?;
    Ok((stdin, BufReader::new(stdout)))
}

pub(crate) fn send_prompt_rpc(
    stdin: &mut impl Write,
    session_id: &str,
    prompt: &str,
) -> Result<(), ControlPlaneError> {
    send_rpc(
        stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [
                    { "type": "text", "text": prompt }
                ]
            }
        }),
    )
}

pub(crate) fn run_acp_session(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    worktree: &str,
    model: &str,
    resumed_session_id: Option<&str>,
    prompt: &str,
    auto_approve: bool,
) -> Result<(String, String, String), ControlPlaneError> {
    let session_id = init_acp_session(stdin, reader, worktree, model, resumed_session_id)?;
    send_prompt_rpc(stdin, &session_id, prompt)?;
    let (assistant_summary, stop_reason) = drain_prompt_turn(stdin, reader, auto_approve)?;
    Ok((session_id, assistant_summary, stop_reason))
}

pub(crate) fn init_acp_session(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    worktree: &str,
    model: &str,
    resumed_session_id: Option<&str>,
) -> Result<String, ControlPlaneError> {
    // 1. Initialize ACP handshake
    send_rpc(
        stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientInfo": { "name": "liberado", "version": "0.1.0" },
                "capabilities": {}
            }
        }),
    )?;
    read_response_until_id(reader, 1)?;

    // 2. Create or reuse session
    let session_id = match resumed_session_id {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        _ => {
            send_rpc(
                stdin,
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/new",
                    "params": {
                        "cwd": worktree,
                        "mcpServers": []
                    }
                }),
            )?;
            let res = read_response_until_id(reader, 2)?;
            res["sessionId"]
                .as_str()
                .ok_or_else(|| {
                    ControlPlaneError::Serialization(serde::de::Error::custom(
                        "missing sessionId in session/new response",
                    ))
                })?
                .to_string()
        }
    };

    // 3. Configure Model
    send_rpc(
        stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/set_config_option",
            "params": {
                "sessionId": session_id,
                "configId": "model",
                "value": model
            }
        }),
    )?;
    let _ = read_response_until_id(reader, 3);

    Ok(session_id)
}

pub(crate) fn drain_prompt_turn(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    auto_approve: bool,
) -> Result<(String, String), ControlPlaneError> {
    let mut assistant_summary = String::new();
    let mut stop_reason = String::new();
    let mut line_buf = String::new();

    loop {
        line_buf.clear();
        let bytes_read = reader
            .read_line(&mut line_buf)
            .map_err(ControlPlaneError::Io)?;
        if bytes_read == 0 {
            break;
        }

        let Ok(msg) = serde_json::from_str::<Value>(&line_buf) else {
            continue;
        };

        if msg.get("method").and_then(|v| v.as_str()) == Some("session/request_permission") {
            if let Some(req_id) = msg.get("id") {
                let outcome = if auto_approve { "accepted" } else { "rejected" };
                let _ = send_rpc(
                    stdin,
                    json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": { "outcome": { "outcome": outcome } }
                    }),
                );
            }
            continue;
        }

        let is_chunk = msg
            .pointer("/params/update/sessionUpdate")
            .and_then(|v| v.as_str())
            == Some("agent_message_chunk");
        if let (true, Some(chunk)) = (
            is_chunk,
            msg.pointer("/params/update/content/text")
                .and_then(|v| v.as_str()),
        ) {
            assistant_summary.push_str(chunk);
        }

        if msg.get("id").and_then(|v| v.as_i64()) == Some(4) {
            if let Some(reason) = msg.pointer("/result/stopReason").and_then(|v| v.as_str()) {
                stop_reason = reason.to_string();
            }
            break;
        }
    }

    Ok((assistant_summary, stop_reason))
}

pub(crate) fn send_rpc(stdin: &mut impl Write, msg: Value) -> Result<(), ControlPlaneError> {
    let line = serde_json::to_string(&msg)?;
    writeln!(stdin, "{line}").map_err(ControlPlaneError::Io)?;
    stdin.flush().map_err(ControlPlaneError::Io)?;
    Ok(())
}

pub(crate) fn read_response_until_id(
    reader: &mut impl BufRead,
    target_id: i64,
) -> Result<Value, ControlPlaneError> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(ControlPlaneError::Io)?;
        if n == 0 {
            return Err(ControlPlaneError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("EOF waiting for rpc id {target_id}"),
            )));
        }

        if let Ok(msg) = serde_json::from_str::<Value>(&line) {
            let matches_target = msg.get("id").and_then(|v| v.as_i64()) == Some(target_id);
            if matches_target {
                if let Some(err) = msg.get("error") {
                    return Err(ControlPlaneError::Serialization(serde::de::Error::custom(
                        format!("RPC error response: {err}"),
                    )));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }
}

pub(crate) fn inspect_git_worktree(worktree: &str) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut commits = Vec::new();

    let path = Path::new(worktree);
    let mut status_cmd = std_command("git");
    status_cmd.args(["status", "--porcelain"]);
    if path.is_dir() {
        status_cmd.current_dir(path);
    }
    let status_output = status_cmd.output().ok().filter(|o| o.status.success());
    if let Some(output) = status_output {
        let out_str = String::from_utf8_lossy(&output.stdout);
        for line in out_str.lines() {
            if line.len() > 3 {
                files.push(line[3..].trim().to_string());
            }
        }
    }

    let mut log_cmd = std_command("git");
    log_cmd.args(["log", "-n", "1", "--format=%H"]);
    if path.is_dir() {
        log_cmd.current_dir(path);
    }
    let log_output = log_cmd.output().ok().filter(|o| o.status.success());
    if let Some(output) = log_output {
        let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !commit.is_empty() {
            commits.push(commit);
        }
    }

    (commits, files)
}
