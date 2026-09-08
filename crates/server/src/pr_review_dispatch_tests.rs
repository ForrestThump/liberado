use super::*;
use liberado_coder_core::pr_review::ObserverIntent;
use liberado_coder_core::{ReviewWorkerConfig, TaskEvent, TaskEventKind, TaskLedger};
use std::collections::BTreeMap;
use std::path::Path;

fn ledger(task_id: &str) -> TaskLedger {
    TaskLedger::new(TaskEvent::new(
        "evt-created",
        task_id,
        TaskEventKind::TaskCreated {
            objective: "observe".into(),
            acceptance_criteria: vec!["slice2".into()],
            worktree: "/tmp".into(),
            branch: String::new(),
            base_ref: "main".into(),
            repo: Some("owner/repo".into()),
        },
    ))
    .unwrap()
}

#[test]
fn shadow_or_missing_eligible_does_not_require_workers() {
    let mut ledger = ledger("pr-owner-repo-1");
    let workers = BTreeMap::new();
    let intents = vec![ObserverIntent::ObserveTip {
        sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
    }];
    maybe_dispatch(DispatchRequest {
        ledger: &mut ledger,
        task_id: "pr-owner-repo-1",
        repository: "owner/repo",
        pr_number: 1,
        head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        base_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        coding_root: Path::new("/tmp"),
        review_workers: &workers,
        intents: &intents,
        shadow: true,
    })
    .unwrap();
    assert!(
        ledger
            .events()
            .iter()
            .all(|e| !matches!(e.payload, TaskEventKind::ReviewCommandIssued { .. }))
    );
}

#[test]
fn disabled_codex_does_not_issue() {
    let mut ledger = ledger("pr-owner-repo-1");
    let mut workers = BTreeMap::new();
    workers.insert(
        "codex".into(),
        ReviewWorkerConfig::Codex {
            executable: "/bin/false".into(),
            enabled: false,
        },
    );
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let intents = vec![ObserverIntent::Eligible { sha: sha.into() }];
    maybe_dispatch(DispatchRequest {
        ledger: &mut ledger,
        task_id: "pr-owner-repo-1",
        repository: "owner/repo",
        pr_number: 1,
        head_sha: sha,
        base_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        coding_root: Path::new("/tmp"),
        review_workers: &workers,
        intents: &intents,
        shadow: false,
    })
    .unwrap();
    assert!(
        ledger
            .events()
            .iter()
            .all(|e| !matches!(e.payload, TaskEventKind::ReviewCommandIssued { .. }))
    );
}
