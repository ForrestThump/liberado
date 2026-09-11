use super::*;
use crate::pr_review::{REVIEW_SCHEMA_VERSION, ReviewResult, WorkerFailure};
use crate::pr_review_port::{
    ReviewInvokeOutcome, ReviewInvokeRequest, ReviewPort, artifact_digest, serialize_review_result,
};
use crate::{TaskEvent, TaskEventKind, TaskLedger};
use std::path::{Path, PathBuf};

fn created(task_id: &str) -> TaskEvent {
    TaskEvent::new(
        "evt-created",
        task_id,
        TaskEventKind::TaskCreated {
            objective: "review".into(),
            acceptance_criteria: vec!["evidence only".into()],
            worktree: "/tmp/review".into(),
            branch: String::new(),
            base_ref: "main".into(),
            repo: Some("owner/repo".into()),
        },
    )
}

struct StubPort(ReviewInvokeOutcome);

impl ReviewPort for StubPort {
    fn invoke(&self, _: &ReviewInvokeRequest) -> ReviewInvokeOutcome {
        self.0.clone()
    }
}

struct PanicPort;

impl ReviewPort for PanicPort {
    fn invoke(&self, _: &ReviewInvokeRequest) -> ReviewInvokeOutcome {
        panic!("issue_review must not invoke when admission refuses the run");
    }
}

fn request(command_id: &str, run_id: &str, sha: &str) -> ReviewInvokeRequest {
    ReviewInvokeRequest {
        task_id: "pr-owner-repo-7".into(),
        command_id: command_id.into(),
        run_id: run_id.into(),
        repository: "owner/repo".into(),
        pr_number: 7,
        base_sha: "b".repeat(40),
        expected_sha: sha.into(),
        workspace: PathBuf::from("/tmp/review-ws"),
        schema_path: PathBuf::from("/tmp/schema.json"),
    }
}

fn clean_result(sha: &str) -> ReviewResult {
    ReviewResult {
        schema: REVIEW_SCHEMA_VERSION.into(),
        reviewed_sha: sha.into(),
        summary: "clean".into(),
        findings: Vec::new(),
    }
}

#[test]
fn command_id_is_stable_and_binds_the_complete_review_key() {
    let sha = "a".repeat(40);
    let id = review_command_id("owner/repo", 7, &sha);
    assert_eq!(id, review_command_id("owner/repo", 7, &sha));
    assert_ne!(id, review_command_id("other/repo", 7, &sha));
    assert_ne!(id, review_command_id("owner/repo", 8, &sha));
    assert_ne!(id, review_command_id("owner/repo", 7, &"b".repeat(40)));
}

#[test]
fn crash_recovery_never_starts_the_same_command_twice() {
    let dir = tempfile::tempdir().unwrap();
    let task_id = "pr-owner-repo-7";
    let command_id = review_command_id("owner/repo", 7, &"a".repeat(40));
    let mut ledger = TaskLedger::create_in(dir.path(), created(task_id)).unwrap();
    assert!(admit_review_command(&mut ledger, task_id, &command_id, &"a".repeat(40)).unwrap());
    assert!(review_run_in_flight(&ledger));
    drop(ledger);

    let mut recovered = TaskLedger::create_in(dir.path(), created(task_id)).unwrap();
    assert!(!admit_review_command(&mut recovered, task_id, &command_id, &"a".repeat(40)).unwrap());
    assert_eq!(
        recovered
            .events()
            .iter()
            .filter(|event| matches!(event.payload, TaskEventKind::ReviewCommandIssued { .. }))
            .count(),
        1
    );
}

#[test]
fn only_the_matching_terminal_run_clears_the_review_fence() {
    let task_id = "pr-owner-repo-7";
    let command_id = "review-current";
    let mut ledger = TaskLedger::new(created(task_id)).unwrap();
    assert!(admit_review_command(&mut ledger, task_id, command_id, &"a".repeat(40)).unwrap());
    ledger
        .append(TaskEvent::new(
            "evt-stale-other",
            task_id,
            TaskEventKind::ReviewStale {
                run_id: "review-other".into(),
                expected_sha: "b".repeat(40),
                observed_sha: "c".repeat(40),
            },
        ))
        .unwrap();
    assert_eq!(
        ledger.project().unwrap().active_run_id.as_deref(),
        Some(command_id)
    );
    ledger
        .append(TaskEvent::new(
            "evt-stale-current",
            task_id,
            TaskEventKind::ReviewStale {
                run_id: command_id.into(),
                expected_sha: "a".repeat(40),
                observed_sha: "b".repeat(40),
            },
        ))
        .unwrap();
    assert_eq!(ledger.project().unwrap().active_run_id, None);
}

#[test]
fn structured_result_is_stored_and_never_becomes_repair_or_approval() {
    let task_id = "pr-owner-repo-7";
    let command_id = "review-finished";
    let sha = "a".repeat(40);
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = TaskLedger::new(created(task_id)).unwrap();
    assert!(admit_review_command(&mut ledger, task_id, command_id, &sha).unwrap());
    let result = clean_result(&sha);
    let bytes = serialize_review_result(&result).unwrap();
    let path = record_review_outcome(
        &mut ledger,
        task_id,
        "codex",
        command_id,
        &sha,
        dir.path(),
        ReviewInvokeOutcome::Finished {
            result: result.clone(),
            artifact_digest: artifact_digest(&bytes),
        },
    )
    .unwrap()
    .expect("result artifact");
    assert_eq!(
        serde_json::from_slice::<ReviewResult>(&std::fs::read(path).unwrap()).unwrap(),
        result
    );
    assert!(ledger.events().iter().any(|event| matches!(
        event.payload,
        TaskEventKind::ReviewRunFinished { ref run_id, .. } if run_id == command_id
    )));
    assert!(!ledger.events().iter().any(|event| matches!(
        event.payload,
        TaskEventKind::WorkerStarted { .. }
            | TaskEventKind::WorkerFinished { .. }
            | TaskEventKind::ReviewApproved { .. }
            | TaskEventKind::ReviewRejected { .. }
    )));
    assert_eq!(ledger.project().unwrap().active_run_id, None);
    assert!(!should_issue_review(&ledger, command_id));
}

#[test]
fn exact_exhaustion_records_worker_unavailable_and_clears_the_fence() {
    let task_id = "pr-owner-repo-7";
    let command_id = "review-exhausted";
    let sha = "a".repeat(40);
    let mut ledger = TaskLedger::new(created(task_id)).unwrap();
    assert!(admit_review_command(&mut ledger, task_id, command_id, &sha).unwrap());
    record_review_outcome(
        &mut ledger,
        task_id,
        "codex",
        command_id,
        &sha,
        Path::new("/unused"),
        ReviewInvokeOutcome::Unavailable {
            failure: WorkerFailure::Exhausted,
        },
    )
    .unwrap();
    assert!(ledger.events().iter().any(|event| matches!(
        &event.payload,
        TaskEventKind::ReviewWorkerUnavailable { run_id, reason, .. }
            if run_id == command_id && reason == "exhausted"
    )));
    assert_eq!(ledger.project().unwrap().active_run_id, None);
}

#[test]
fn failed_review_does_not_admit_a_fallback_run() {
    let task_id = "pr-owner-repo-7";
    let command_id = "review-failed";
    let sha = "a".repeat(40);
    let mut ledger = TaskLedger::new(created(task_id)).unwrap();
    assert!(admit_review_command(&mut ledger, task_id, command_id, &sha).unwrap());
    record_review_outcome(
        &mut ledger,
        task_id,
        "codex",
        command_id,
        &sha,
        Path::new("/unused"),
        ReviewInvokeOutcome::Failed {
            reason: "review failed: model_failure".into(),
        },
    )
    .unwrap();
    assert!(
        !admit_review_run(
            &mut ledger,
            task_id,
            command_id,
            &format!("{command_id}:open_code"),
            "open_code",
        )
        .unwrap()
    );
}

#[test]
fn issue_review_records_the_first_command_and_its_result() {
    let task_id = "pr-owner-repo-7";
    let command_id = "review-first";
    let sha = "a".repeat(40);
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = TaskLedger::new(created(task_id)).unwrap();
    let result = clean_result(&sha);
    let outcome = issue_review(
        &mut ledger,
        &StubPort(ReviewInvokeOutcome::Finished {
            result: result.clone(),
            artifact_digest: artifact_digest(b"unused"),
        }),
        "codex",
        &request(command_id, command_id, &sha),
        dir.path(),
    )
    .unwrap()
    .expect("first command is admitted");
    assert!(matches!(outcome, ReviewInvokeOutcome::Finished { .. }));
    assert!(ledger.events().iter().any(|event| matches!(
        &event.payload,
        TaskEventKind::ReviewCommandIssued { command_id: id, .. } if id == command_id
    )));
    assert!(ledger.events().iter().any(|event| matches!(
        &event.payload,
        TaskEventKind::ReviewRunFinished { run_id, .. } if run_id == command_id
    )));
}

#[test]
fn issue_review_admits_a_fallback_run_after_exhaustion() {
    let task_id = "pr-owner-repo-7";
    let command_id = "review-fallback";
    let run_id = format!("{command_id}:open_code");
    let sha = "a".repeat(40);
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = TaskLedger::new(created(task_id)).unwrap();
    assert!(admit_review_command(&mut ledger, task_id, command_id, &sha).unwrap());
    record_review_outcome(
        &mut ledger,
        task_id,
        "codex",
        command_id,
        &sha,
        Path::new("/unused"),
        ReviewInvokeOutcome::Unavailable {
            failure: WorkerFailure::Exhausted,
        },
    )
    .unwrap();
    let outcome = issue_review(
        &mut ledger,
        &StubPort(ReviewInvokeOutcome::Finished {
            result: clean_result(&sha),
            artifact_digest: artifact_digest(b"unused"),
        }),
        "open_code",
        &request(command_id, &run_id, &sha),
        dir.path(),
    )
    .unwrap()
    .expect("fallback run is admitted");
    assert!(matches!(outcome, ReviewInvokeOutcome::Finished { .. }));
    assert!(ledger.events().iter().any(|event| matches!(
        &event.payload,
        TaskEventKind::ReviewRunStarted { run_id: id, worker_id, .. }
            if id == &run_id && worker_id == "open_code"
    )));
}

#[test]
fn issue_review_is_a_no_op_when_the_command_already_finished() {
    let task_id = "pr-owner-repo-7";
    let command_id = "review-done";
    let sha = "a".repeat(40);
    let mut ledger = TaskLedger::new(created(task_id)).unwrap();
    assert!(admit_review_command(&mut ledger, task_id, command_id, &sha).unwrap());
    record_review_outcome(
        &mut ledger,
        task_id,
        "codex",
        command_id,
        &sha,
        Path::new("/unused"),
        ReviewInvokeOutcome::Failed {
            reason: "review failed: model_failure".into(),
        },
    )
    .unwrap();
    assert!(
        issue_review(
            &mut ledger,
            &PanicPort,
            "open_code",
            &request(command_id, &format!("{command_id}:open_code"), &sha),
            Path::new("/unused"),
        )
        .unwrap()
        .is_none()
    );
}
