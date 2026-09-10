use std::collections::BTreeMap;
use std::path::Path;

use super::*;
use crate::ReviewWorkerConfig;
use crate::TaskEvent;
use crate::TaskEventKind;
use crate::TaskLedger;
use crate::pr_review::WorkerFailure;
use crate::pr_review_admission::{admit_review_command, record_review_outcome};
use crate::pr_review_port::ReviewInvokeOutcome;

fn ledger(task_id: &str) -> TaskLedger {
    TaskLedger::new(TaskEvent::new(
        "evt-created",
        task_id,
        TaskEventKind::TaskCreated {
            objective: "review".into(),
            acceptance_criteria: vec!["route".into()],
            worktree: "/tmp".into(),
            branch: String::new(),
            base_ref: "main".into(),
            repo: Some("owner/repo".into()),
        },
    ))
    .unwrap()
}

fn workers() -> BTreeMap<String, ReviewWorkerConfig> {
    let mut workers = BTreeMap::new();
    workers.insert(
        "codex".into(),
        ReviewWorkerConfig::Codex {
            executable: "codex".into(),
            enabled: true,
        },
    );
    workers.insert(
        "open_code".into(),
        ReviewWorkerConfig::OpenCode {
            executable: "opencode".into(),
            model: crate::OPENCODE_NAMED_REVIEW_MODEL.into(),
            permission_mode: "deny_writes".into(),
            pricing_policy: "named".into(),
            enabled: true,
        },
    );
    workers
}

fn order() -> Vec<String> {
    vec!["codex".into(), "open_code".into()]
}

#[test]
fn first_attempt_selects_codex() {
    let ledger = ledger("pr-1");
    let workers = workers();
    let order = order();
    let (id, worker) = next_review_worker(&ledger, "cmd", &order, &workers).expect("codex");
    assert_eq!(id, "codex");
    assert!(matches!(worker, ReviewWorkerConfig::Codex { .. }));
}

#[test]
fn exhausted_codex_advances_to_opencode() {
    let mut book = ledger("pr-1");
    let command = "cmd";
    admit_review_command(&mut book, "pr-1", command, &"a".repeat(40)).unwrap();
    record_review_outcome(
        &mut book,
        "pr-1",
        "codex",
        command,
        &"a".repeat(40),
        Path::new("/unused"),
        ReviewInvokeOutcome::Unavailable {
            failure: WorkerFailure::Exhausted,
        },
    )
    .unwrap();
    let workers = workers();
    let order = order();
    let (id, _) = next_review_worker(&book, command, &order, &workers).expect("open_code");
    assert_eq!(id, "open_code");
    assert_eq!(review_run_id(command, "open_code"), "cmd:open_code");
}

#[test]
fn failed_codex_does_not_start_opencode() {
    let mut book = ledger("pr-1");
    let command = "cmd";
    admit_review_command(&mut book, "pr-1", command, &"a".repeat(40)).unwrap();
    record_review_outcome(
        &mut book,
        "pr-1",
        "codex",
        command,
        &"a".repeat(40),
        Path::new("/unused"),
        ReviewInvokeOutcome::Failed {
            reason: "review failed: model_failure".into(),
        },
    )
    .unwrap();
    let workers = workers();
    let order = order();
    assert!(next_review_worker(&book, command, &order, &workers).is_none());
}

#[test]
fn disabled_opencode_is_not_selected() {
    let mut book = ledger("pr-1");
    let command = "cmd";
    admit_review_command(&mut book, "pr-1", command, &"a".repeat(40)).unwrap();
    record_review_outcome(
        &mut book,
        "pr-1",
        "codex",
        command,
        &"a".repeat(40),
        Path::new("/unused"),
        ReviewInvokeOutcome::Unavailable {
            failure: WorkerFailure::RateLimited,
        },
    )
    .unwrap();
    let mut workers = workers();
    if let ReviewWorkerConfig::OpenCode { enabled, .. } = workers.get_mut("open_code").unwrap() {
        *enabled = false;
    }
    let order = order();
    assert!(next_review_worker(&book, command, &order, &workers).is_none());
}
