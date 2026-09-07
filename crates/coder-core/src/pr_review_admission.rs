//! Slice 2 admission: fence one review command per review key and record outcomes.

use std::path::{Path, PathBuf};

use crate::TaskEvent;
use crate::TaskEventKind;
use crate::TaskLedger;
use crate::pr_review::{POLICY_VERSION, WorkerFailure};
use crate::pr_review_port::{
    ReviewInvokeOutcome, ReviewInvokeRequest, ReviewPort, artifact_digest, serialize_review_result,
};

/// Deterministic command/run id for repository + PR + SHA + policy version.
pub fn review_command_id(repository: &str, pr_number: u64, head_sha: &str) -> String {
    format!("review-cmd:{POLICY_VERSION}:{repository}:{pr_number}:{head_sha}")
}

fn has_command_issue(ledger: &TaskLedger, command_id: &str) -> bool {
    ledger.events().iter().any(|event| {
        matches!(
            &event.payload,
            TaskEventKind::ReviewCommandIssued { command_id: id, .. } if id == command_id
        ) || event.command_id.as_deref() == Some(command_id)
    })
}

/// True when any review/repair run fence is still open on the projected record.
pub fn review_run_in_flight(ledger: &TaskLedger) -> bool {
    ledger
        .project()
        .ok()
        .and_then(|record| record.active_run_id)
        .is_some()
}

pub fn should_issue_review(ledger: &TaskLedger, command_id: &str) -> bool {
    !has_command_issue(ledger, command_id) && !review_run_in_flight(ledger)
}

fn append(
    ledger: &mut TaskLedger,
    task_id: &str,
    command: String,
    kind: TaskEventKind,
) -> Result<(), String> {
    ledger
        .append(TaskEvent::new(format!("evt-{command}"), task_id, kind).with_command_id(command))
        .map_err(|e| e.to_string())
}

/// Record `ReviewCommandIssued` once. Returns false when the fence already exists.
pub fn admit_review_command(
    ledger: &mut TaskLedger,
    task_id: &str,
    command_id: &str,
    head_sha: &str,
) -> Result<bool, String> {
    if !should_issue_review(ledger, command_id) {
        return Ok(false);
    }
    append(
        ledger,
        task_id,
        command_id.to_string(),
        TaskEventKind::ReviewCommandIssued {
            command_id: command_id.to_string(),
            head_sha: head_sha.to_string(),
        },
    )?;
    Ok(true)
}

fn failure_reason(failure: WorkerFailure) -> &'static str {
    match failure {
        WorkerFailure::Exhausted => "exhausted",
        WorkerFailure::RateLimited => "rate_limited",
        WorkerFailure::Auth => "auth",
        WorkerFailure::Permission => "permission",
        WorkerFailure::Timeout => "timeout",
        WorkerFailure::ModelFailure => "model_failure",
    }
}

/// Append the terminal review fact. Finished outcomes also persist the structured artifact.
pub fn record_review_outcome(
    ledger: &mut TaskLedger,
    task_id: &str,
    command_id: &str,
    worker_id: &str,
    head_sha: &str,
    artifact_dir: &Path,
    outcome: ReviewInvokeOutcome,
) -> Result<Option<PathBuf>, String> {
    match outcome {
        ReviewInvokeOutcome::Finished {
            result,
            artifact_digest: digest,
        } => {
            let bytes = serialize_review_result(&result)?;
            let digest = if digest.is_empty() {
                artifact_digest(&bytes)
            } else {
                digest
            };
            let path = artifact_dir.join(format!("{digest}.json"));
            std::fs::create_dir_all(artifact_dir).map_err(|e| e.to_string())?;
            std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
            append(
                ledger,
                task_id,
                format!("{command_id}:finished"),
                TaskEventKind::ReviewRunFinished {
                    run_id: command_id.into(),
                    worker_id: worker_id.into(),
                    head_sha: head_sha.into(),
                    artifact_digest: digest,
                },
            )?;
            Ok(Some(path))
        }
        ReviewInvokeOutcome::Unavailable { failure } => {
            append(
                ledger,
                task_id,
                format!("{command_id}:unavailable"),
                TaskEventKind::ReviewWorkerUnavailable {
                    run_id: command_id.into(),
                    worker_id: worker_id.into(),
                    reason: failure_reason(failure).into(),
                },
            )?;
            Ok(None)
        }
        ReviewInvokeOutcome::Failed { reason } => {
            append(
                ledger,
                task_id,
                format!("{command_id}:unavailable"),
                TaskEventKind::ReviewWorkerUnavailable {
                    run_id: command_id.into(),
                    worker_id: worker_id.into(),
                    reason,
                },
            )?;
            Ok(None)
        }
        ReviewInvokeOutcome::Stale { expected, observed } => {
            append(
                ledger,
                task_id,
                format!("{command_id}:stale"),
                TaskEventKind::ReviewStale {
                    run_id: command_id.into(),
                    expected_sha: expected,
                    observed_sha: observed,
                },
            )?;
            Ok(None)
        }
    }
}

/// Issue at most one review command, invoke the port, and append the terminal fact.
pub fn issue_review(
    ledger: &mut TaskLedger,
    port: &dyn ReviewPort,
    worker_id: &str,
    request: &ReviewInvokeRequest,
    artifact_dir: &Path,
) -> Result<Option<ReviewInvokeOutcome>, String> {
    if !admit_review_command(
        ledger,
        &request.task_id,
        &request.command_id,
        &request.expected_sha,
    )? {
        return Ok(None);
    }
    let outcome = port.invoke(request);
    record_review_outcome(
        ledger,
        &request.task_id,
        &request.command_id,
        worker_id,
        &request.expected_sha,
        artifact_dir,
        outcome.clone(),
    )?;
    Ok(Some(outcome))
}

#[cfg(test)]
#[path = "pr_review_admission_tests.rs"]
mod tests;
