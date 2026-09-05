//! OpenCode worker adapter communicating via Agent Client Protocol (ACP) over stdio.
//!
//! OpenCode runs as an external coding worker speaking ACP over stdio JSON-RPC.
//! This adapter manages session handshakes, model configuration (e.g. OpenRouter DeepSeek),
//! permission auto-approvals, tool turn collection, and git artifact inspection.

use super::{ControlPlaneError, TaskEvent, WorkerRunResult, WorkerStatus};
use liberado_common::process::std_command;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

mod adapter;
mod process_tree;
pub(crate) use adapter::GitSnapshot;
pub use adapter::{OpenCodeWorker, OpenCodeWorkerConfig};

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
    let completed = stop_reason == "end_turn";
    let status = if completed {
        WorkerStatus::Completed
    } else {
        WorkerStatus::Failed
    };

    WorkerRunResult {
        status,
        summary: assistant_summary.trim().to_string(),
        commits,
        files_changed,
        tests_run: 0,
        tests_passed: 0,
        blocking_issue: if completed {
            None
        } else {
            Some(format!("worker stopped with reason '{stop_reason}'"))
        },
        recommended_next_action: None,
        external_session_id: Some(session_id),
    }
}

fn failed_worker_result(message: &str, session_id: Option<String>) -> WorkerRunResult {
    WorkerRunResult {
        status: WorkerStatus::Failed,
        summary: String::new(),
        commits: Vec::new(),
        files_changed: Vec::new(),
        tests_run: 0,
        tests_passed: 0,
        blocking_issue: Some(message.to_string()),
        recommended_next_action: None,
        external_session_id: session_id,
    }
}

fn next_run_id() -> String {
    let sequence = RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("run-{}-{sequence}", chrono::Utc::now().timestamp_millis())
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
                "clientCapabilities": {}
            }
        }),
    )?;
    read_response_until_id(reader, 1)?;

    // 2. Create or restore the session in this newly spawned ACP process.
    let session_id = match resumed_session_id {
        Some(id) if !id.trim().is_empty() => {
            send_rpc(
                stdin,
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/load",
                    "params": {
                        "sessionId": id,
                        "cwd": worktree,
                        "mcpServers": []
                    }
                }),
            )?;
            match read_response_until_id(reader, 2) {
                Ok(_) => id.to_string(),
                Err(_) => create_acp_session(stdin, reader, worktree, 5)?,
            }
        }
        _ => create_acp_session(stdin, reader, worktree, 2)?,
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
    read_response_until_id(reader, 3)?;

    Ok(session_id)
}

fn create_acp_session(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    worktree: &str,
    request_id: i64,
) -> Result<String, ControlPlaneError> {
    send_rpc(
        stdin,
        json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "session/new",
            "params": {
                "cwd": worktree,
                "mcpServers": []
            }
        }),
    )?;
    let response = read_response_until_id(reader, request_id)?;
    response["sessionId"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ControlPlaneError::Protocol("session/new response omitted sessionId".into()))
}

pub(crate) fn drain_prompt_turn(
    stdin: &mut impl Write,
    reader: &mut impl BufRead,
    auto_approve: bool,
) -> Result<(String, String), ControlPlaneError> {
    let mut assistant_summary = String::new();
    let mut line_buf = String::new();

    let stop_reason = loop {
        line_buf.clear();
        let bytes_read = reader
            .read_line(&mut line_buf)
            .map_err(ControlPlaneError::Io)?;
        if bytes_read == 0 {
            return Err(ControlPlaneError::Protocol(
                "ACP process closed stdout before session/prompt completed".into(),
            ));
        }

        let Ok(msg) = serde_json::from_str::<Value>(&line_buf) else {
            continue;
        };

        if msg.get("method").and_then(|v| v.as_str()) == Some("session/request_permission") {
            if let Some(req_id) = msg.get("id") {
                let result = permission_result(&msg, auto_approve);
                send_rpc(
                    stdin,
                    json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": result
                    }),
                )?;
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
            if let Some(error) = msg.get("error") {
                return Err(ControlPlaneError::Protocol(format!(
                    "session/prompt failed: {error}"
                )));
            }
            if let Some(reason) = msg.pointer("/result/stopReason").and_then(|v| v.as_str()) {
                break reason.to_string();
            } else {
                return Err(ControlPlaneError::Protocol(
                    "session/prompt response omitted stopReason".into(),
                ));
            }
        }
    };

    Ok((assistant_summary, stop_reason))
}

fn permission_result(message: &Value, auto_approve: bool) -> Value {
    if auto_approve {
        let options = message.pointer("/params/options").and_then(Value::as_array);
        let allow = options.and_then(|options| {
            options
                .iter()
                .find(|option| option.get("kind").and_then(Value::as_str) == Some("allow_always"))
                .or_else(|| {
                    options.iter().find(|option| {
                        option.get("kind").and_then(Value::as_str) == Some("allow_once")
                    })
                })
        });
        if let Some(option_id) = allow
            .and_then(|option| option.get("optionId"))
            .and_then(Value::as_str)
        {
            return json!({ "outcome": { "outcome": "selected", "optionId": option_id } });
        }
    }
    json!({ "outcome": { "outcome": "cancelled" } })
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

pub(crate) fn capture_git_snapshot(worktree: &str) -> Result<GitSnapshot, ControlPlaneError> {
    let path = Path::new(worktree);
    if !path.is_dir() {
        return Err(ControlPlaneError::Protocol(format!(
            "worker worktree does not exist: {worktree}"
        )));
    }
    let head = git_output(path, &["rev-parse", "HEAD"])?;
    Ok(GitSnapshot {
        head: head.trim().to_string(),
    })
}

pub(crate) fn inspect_git_worktree(
    worktree: &str,
    baseline: &GitSnapshot,
) -> Result<(Vec<String>, Vec<String>), ControlPlaneError> {
    let path = Path::new(worktree);
    if !path.is_dir() {
        return Err(ControlPlaneError::Protocol(format!(
            "worker worktree does not exist: {worktree}"
        )));
    }

    let range = format!("{}..HEAD", baseline.head);
    let commits = git_output(path, &["rev-list", "--reverse", &range])?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();

    let mut files = BTreeSet::new();
    for file in git_output(path, &["diff", "--name-only", &range])?.lines() {
        if !file.trim().is_empty() {
            files.insert(file.trim().to_string());
        }
    }
    for line in git_output(path, &["status", "--porcelain"])?.lines() {
        if line.len() > 3 {
            let changed = line[3..].trim();
            if let Some((_, destination)) = changed.rsplit_once(" -> ") {
                files.insert(destination.to_string());
            } else if !changed.is_empty() {
                files.insert(changed.to_string());
            }
        }
    }

    Ok((commits, files.into_iter().collect()))
}

fn git_output(path: &Path, args: &[&str]) -> Result<String, ControlPlaneError> {
    let output = std_command("git").args(args).current_dir(path).output()?;
    if !output.status.success() {
        return Err(ControlPlaneError::Protocol(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
