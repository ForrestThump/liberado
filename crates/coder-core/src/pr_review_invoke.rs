//! Shared process invoke for one-shot review ports.

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::ExitStatus;

use sha2::{Digest, Sha256};

use super::{ReviewInvokeOutcome, ReviewInvokeRequest};
use crate::pr_review::{
    MAX_RAW_OUTPUT_BYTES, ReviewResult, ReviewResultError, WorkerFailure, valid_full_sha,
};

pub(super) fn apply_review_env(
    command: &mut std::process::Command,
    extra_env: &[(String, String)],
) {
    command.envs(extra_env.iter().map(|(name, value)| (name, value)));
    for (name, _) in std::env::vars_os() {
        if is_forge_environment(&name) {
            command.env_remove(name);
        }
    }
    for (name, _) in extra_env {
        if is_forge_environment(OsStr::new(name)) {
            command.env_remove(name);
        }
    }
}

pub(super) fn invoke_process(
    request: &ReviewInvokeRequest,
    run: impl FnOnce() -> Result<CapturedOutput, String>,
    parse: fn(&str, &str) -> Result<ReviewResult, ReviewResultError>,
    classify: fn(&str) -> WorkerFailure,
) -> ReviewInvokeOutcome {
    if let Some(reason) = invalid_request(request) {
        return ReviewInvokeOutcome::Failed { reason };
    }
    if let Some(outcome) = workspace_not_ready(request) {
        return outcome;
    }
    let output = match run() {
        Ok(output) => output,
        Err(reason) => return ReviewInvokeOutcome::Failed { reason },
    };
    if let Some(outcome) = workspace_changed(request) {
        return outcome;
    }
    classify_output(request, output, parse, classify)
}

fn workspace_not_ready(request: &ReviewInvokeRequest) -> Option<ReviewInvokeOutcome> {
    let observed = match workspace_head(&request.workspace) {
        Ok(sha) => sha,
        Err(reason) => return Some(ReviewInvokeOutcome::Failed { reason }),
    };
    if observed != request.expected_sha {
        return Some(stale(request, observed));
    }
    if !commit_exists(&request.workspace, &request.base_sha) {
        return Some(ReviewInvokeOutcome::Failed {
            reason: "base SHA is not a commit in the pinned checkout".into(),
        });
    }
    if !workspace_is_clean(&request.workspace) {
        return Some(ReviewInvokeOutcome::Failed {
            reason: "pinned review checkout is not clean".into(),
        });
    }
    None
}

fn workspace_changed(request: &ReviewInvokeRequest) -> Option<ReviewInvokeOutcome> {
    let observed = match workspace_head(&request.workspace) {
        Ok(sha) => sha,
        Err(reason) => return Some(ReviewInvokeOutcome::Failed { reason }),
    };
    if observed != request.expected_sha {
        return Some(stale(request, observed));
    }
    if !workspace_is_clean(&request.workspace) {
        return Some(ReviewInvokeOutcome::Failed {
            reason: "review worker modified the pinned checkout".into(),
        });
    }
    None
}

pub(super) fn parse_codex_or_fail(
    stdout: &str,
    expected_sha: &str,
) -> Result<ReviewResult, ReviewResultError> {
    crate::pr_review::parse_codex_success(stdout, expected_sha)
}

fn invalid_request(request: &ReviewInvokeRequest) -> Option<String> {
    if request.command_id.trim().is_empty() || request.run_id.trim().is_empty() {
        return Some("review command and run IDs must be nonempty".into());
    }
    if request.task_id.trim().is_empty()
        || request.repository.trim().is_empty()
        || request.pr_number == 0
    {
        return Some("review identity is incomplete".into());
    }
    if !valid_full_sha(&request.base_sha) || !valid_full_sha(&request.expected_sha) {
        return Some("review base and expected SHAs must be full SHAs".into());
    }
    if !request.schema_path.is_file() {
        return Some("review result schema is missing".into());
    }
    None
}

fn stale(request: &ReviewInvokeRequest, observed: String) -> ReviewInvokeOutcome {
    ReviewInvokeOutcome::Stale {
        expected: request.expected_sha.clone(),
        observed,
    }
}

fn classify_output(
    request: &ReviewInvokeRequest,
    output: CapturedOutput,
    parse: fn(&str, &str) -> Result<ReviewResult, ReviewResultError>,
    classify: fn(&str) -> WorkerFailure,
) -> ReviewInvokeOutcome {
    if output.overflow {
        return ReviewInvokeOutcome::Failed {
            reason: format!("review output exceeded {MAX_RAW_OUTPUT_BYTES} bytes"),
        };
    }
    if !output.status.success() {
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        let failure = classify(&combined);
        return match failure {
            WorkerFailure::Exhausted | WorkerFailure::RateLimited => {
                ReviewInvokeOutcome::Unavailable { failure }
            }
            _ => ReviewInvokeOutcome::Failed {
                reason: format!("review failed: {}", failure_name(failure)),
            },
        };
    }
    let stdout = match std::str::from_utf8(&output.stdout) {
        Ok(stdout) => stdout,
        Err(_) => {
            return ReviewInvokeOutcome::Failed {
                reason: "review worker returned non-UTF-8 output".into(),
            };
        }
    };
    match parse(stdout, &request.expected_sha) {
        Ok(result) => finished(result),
        Err(ReviewResultError::Malformed) => ReviewInvokeOutcome::Failed {
            reason: "review worker returned malformed result JSON".into(),
        },
        Err(ReviewResultError::StaleSha) => stale(request, reviewed_sha(stdout)),
    }
}

fn finished(result: ReviewResult) -> ReviewInvokeOutcome {
    match serialize_review_result(&result) {
        Ok(bytes) => ReviewInvokeOutcome::Finished {
            result,
            artifact_digest: artifact_digest(&bytes),
        },
        Err(reason) => ReviewInvokeOutcome::Failed { reason },
    }
}

fn reviewed_sha(output: &str) -> String {
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|value| value["reviewed_sha"].as_str().map(str::to_owned))
        .filter(|sha| valid_full_sha(sha))
        .unwrap_or_else(|| "invalid-review-result-sha".into())
}

fn failure_name(failure: WorkerFailure) -> &'static str {
    match failure {
        WorkerFailure::Exhausted => "exhausted",
        WorkerFailure::RateLimited => "rate_limited",
        WorkerFailure::Auth => "auth",
        WorkerFailure::Permission => "permission",
        WorkerFailure::Timeout => "timeout",
        WorkerFailure::ModelFailure => "model_failure",
    }
}

pub fn serialize_review_result(result: &ReviewResult) -> Result<Vec<u8>, String> {
    serde_json::to_vec(result)
        .map_err(|error| format!("could not serialize review result: {error}"))
}

pub fn artifact_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn workspace_head(workspace: &Path) -> Result<String, String> {
    let output = liberado_common::process::std_command("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(workspace)
        .output()
        .map_err(|error| format!("could not inspect pinned checkout HEAD: {error}"))?;
    if !output.status.success() {
        return Err("could not inspect pinned checkout HEAD".into());
    }
    String::from_utf8(output.stdout)
        .map(|sha| sha.trim().to_string())
        .map_err(|_| "pinned checkout HEAD was not UTF-8".into())
}

fn commit_exists(workspace: &Path, sha: &str) -> bool {
    liberado_common::process::std_command("git")
        .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .current_dir(workspace)
        .status()
        .is_ok_and(|status| status.success())
}

fn workspace_is_clean(workspace: &Path) -> bool {
    liberado_common::process::std_command("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(workspace)
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.is_empty())
}

pub(crate) fn is_forge_environment(name: &OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_uppercase();
    name.starts_with("GITHUB_")
        || name.starts_with("GH_")
        || name.starts_with("LIBERADO_GITHUB_")
        || matches!(
            name.as_str(),
            "GIT_ASKPASS"
                | "GIT_SSH"
                | "GIT_SSH_COMMAND"
                | "SSH_ASKPASS"
                | "SSH_AUTH_SOCK"
                | "SSH_AGENT_PID"
        )
        || forge_secret_name(&name)
}

fn forge_secret_name(name: &str) -> bool {
    ["GITLAB", "GLAB", "BITBUCKET", "GITEA", "FORGEJO"]
        .iter()
        .any(|forge| name.contains(forge))
        && ["TOKEN", "SECRET", "PASSWORD", "PRIVATE_KEY", "AUTH"]
            .iter()
            .any(|secret| name.contains(secret))
}

pub(super) struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    overflow: bool,
}

pub(super) fn capture(mut command: std::process::Command) -> Result<CapturedOutput, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start Codex review: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Codex stdout was not captured".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Codex stderr was not captured".to_string())?;
    let (stdout, stderr) = std::thread::scope(|scope| {
        let stdout = scope.spawn(move || read_capped(stdout));
        let stderr = scope.spawn(move || read_capped(stderr));
        (stdout.join(), stderr.join())
    });
    let status = child
        .wait()
        .map_err(|error| format!("could not wait for Codex review: {error}"))?;
    let (stdout, stdout_overflow) = stdout
        .map_err(|_| "Codex stdout reader failed".to_string())?
        .map_err(|error| format!("could not read Codex stdout: {error}"))?;
    let (stderr, stderr_overflow) = stderr
        .map_err(|_| "Codex stderr reader failed".to_string())?
        .map_err(|error| format!("could not read Codex stderr: {error}"))?;
    let combined_overflow = stdout.len().saturating_add(stderr.len()) > MAX_RAW_OUTPUT_BYTES;
    Ok(CapturedOutput {
        status,
        stdout,
        stderr,
        overflow: stdout_overflow || stderr_overflow || combined_overflow,
    })
}

fn read_capped(mut reader: impl Read) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut overflow = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_RAW_OUTPUT_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(remaining)]);
        overflow |= read > remaining;
    }
    Ok((kept, overflow))
}
