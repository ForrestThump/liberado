//! Deterministic coding-domain verifiers (backend-owned, not model-owned).
//!
//! These are the coding pack's `Verifier` implementations: real git status, path-independent
//! validation re-run, and mapping progress fatals to terminal errors. They intentionally do **not**
//! go through model-facing command policy.

use chrono::Utc;
use liberado_coder_core::{CoderCommandConfig, CoderError, CoderEvent, CoderRunRequest};
use liberado_coder_sandbox::CommandRequest;
use liberado_common::Outcome;
use tokio::process::Command;

use crate::progress::ProgressFatal;
use crate::trace::{self, EventLog};

pub fn command_request(command: &CoderCommandConfig) -> CommandRequest {
    CommandRequest {
        program: command.program.clone(),
        args: command.args.clone(),
        env: command.env.clone(),
        timeout_secs: command.timeout_secs,
        output_max_bytes: command.output_max_bytes,
    }
}

pub async fn fail_with_progress_fatal(
    request: &CoderRunRequest,
    session_id: &str,
    events: &EventLog,
    fatal: ProgressFatal,
) -> CoderError {
    trace::push_event(
        events,
        CoderEvent::LoopGuardTriggered {
            guard: fatal.guard_name().to_string(),
            action: "fail_run".to_string(),
            at: Utc::now(),
        },
    );
    trace::push_event(
        events,
        CoderEvent::SessionFinished {
            outcome: Outcome::Failed,
            at: Utc::now(),
        },
    );
    let _ = trace::write_trace(request, session_id, trace::snapshot_events(events), None).await;
    match fatal {
        ProgressFatal::ReadOnlyStall { .. } | ProgressFatal::SameToolChurn { .. } => {
            CoderError::NoChanges
        }
        ProgressFatal::ValidationChurn { .. } => CoderError::Validation(fatal.message()),
    }
}

// (The legacy single-command `run_validation_gate`/`validation_summary` pair lived here until
// 2026-07-11 — superseded by `verify_pipeline`, which synthesizes a one-element pipeline from a
// bare `validation_command` (verifiers.md §5.3) — deleted once nothing referenced them.)

pub async fn changed_files(workspace_root: &str) -> Result<Vec<String>, CoderError> {
    // `-uall` lists files inside new untracked dirs (`src/main.rs`) instead of only `src/`.
    let output = Command::new("git")
        .args(["status", "--porcelain", "-uall"])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git status: {e}")))?;
    if !output.status.success() {
        return Err(CoderError::Backend(format!(
            "git status exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().filter_map(parse_status_path).collect())
}

pub fn parse_status_path(line: &str) -> Option<String> {
    if line.len() < 4 {
        return None;
    }
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }
    let path = path
        .rsplit_once(" -> ")
        .map(|(_, new_path)| new_path)
        .unwrap_or(path);
    Some(path.trim_matches('"').to_string())
}
