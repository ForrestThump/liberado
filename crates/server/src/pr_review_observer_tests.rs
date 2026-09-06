//! Observer unit tests. GitHub stays out: IO is GET-only and uninvoked here.

use super::*;
use liberado_coder_core::pr_review::{
    CheckConclusion, ObserverIntent, PullRequestSnapshot, ReviewPolicy, ShaChecks,
    controller_lease_required, reconcile_snapshot,
};
use liberado_coder_core::{TaskEvent, TaskEventKind, TaskLedger};
use liberado_config::{ShepherdProjectConfig, Topology};

fn policy(shadow: bool) -> ReviewPolicy {
    ReviewPolicy {
        controller: if shadow {
            "grok-bot".into()
        } else {
            "liberado-shepherd".into()
        },
        check_names: vec!["CI".into()],
        shadow,
    }
}

fn project(controller: &str) -> ShepherdProjectConfig {
    ShepherdProjectConfig {
        name: "example".into(),
        repository: "owner/repo".into(),
        coding_project: "example".into(),
        base_branch: "main".into(),
        profile: "coding-unattended".into(),
        check_names: vec!["CI".into()],
        max_kickbacks: None,
        cold_reviews: Some(0),
        cold_review_max_turns: None,
        max_concurrent_goals: None,
        poll_seconds: None,
        controller: Some(controller.into()),
        review_profile: Some("coding-review-unattended".into()),
        gate: Some("ready_and_green_tip".into()),
        auth: Some("github".into()),
    }
}

fn pr(sha: &str) -> PullRequestSnapshot {
    PullRequestSnapshot {
        repository: "owner/repo".into(),
        number: 9,
        head_sha: sha.into(),
        open: true,
        draft: false,
        fork_head: false,
    }
}

fn checks(sha: &str) -> ShaChecks {
    ShaChecks {
        sha: sha.into(),
        checks: vec![("CI".into(), CheckConclusion::Success)],
    }
}

fn ledger(task_id: &str) -> TaskLedger {
    TaskLedger::new(TaskEvent::new(
        "evt-created",
        task_id,
        TaskEventKind::TaskCreated {
            objective: "observe".into(),
            acceptance_criteria: vec!["no dispatch".into()],
            worktree: "/tmp".into(),
            branch: String::new(),
            base_ref: "main".into(),
            repo: Some("owner/repo".into()),
        },
    ))
    .unwrap()
}

#[test]
fn disabled_review_starts_no_task() {
    let topology = Topology::default();
    assert!(!topology.shepherd.review.enabled);
    assert!(spawn(&topology).is_none());
}

#[tokio::test]
async fn enabled_review_starts_a_background_task() {
    let mut topology = Topology::default();
    topology.shepherd.review.enabled = true;
    topology.shepherd.review.poll_seconds = 3600;
    topology.shepherd.review.reconcile_on_start = false;
    let handle = spawn(&topology).expect("enabled observer starts a task");
    handle.abort();
}

#[test]
fn shadow_controller_holds_no_lease() {
    let grok = project("grok-bot");
    let policy = review_policy(&grok);
    assert!(policy.shadow);
    assert!(!controller_lease_required(&policy));
    let shepherd = project("liberado-shepherd");
    assert!(controller_lease_required(&review_policy(&shepherd)));
}

#[test]
fn two_repositories_do_not_share_a_task_id() {
    assert_ne!(
        shepherd_task_id(Some("one/repo"), 1),
        shepherd_task_id(Some("two/repo"), 1)
    );
}

#[test]
fn eligible_intent_is_not_written_and_restart_is_stable() {
    let sha = "a".repeat(40);
    let cycle = ReviewCycle {
        armed_sha: Some(sha.clone()),
        accepted_sha: None,
    };
    let intents = reconcile_snapshot(
        &policy(false),
        &pr(&sha),
        &cycle,
        &checks(&sha),
        None,
        false,
    );
    assert!(
        intents
            .iter()
            .any(|intent| matches!(intent, ObserverIntent::Eligible { .. }))
    );
    assert!(
        intent_event(
            "owner/repo",
            9,
            &ObserverIntent::Eligible { sha: sha.clone() }
        )
        .is_none()
    );
    let mut first = ledger("pr-owner-repo-9");
    record_intents(&mut first, "pr-owner-repo-9", "owner/repo", 9, &intents).unwrap();
    let count = first.events().len();
    record_intents(&mut first, "pr-owner-repo-9", "owner/repo", 9, &intents).unwrap();
    assert_eq!(first.events().len(), count);
    assert!(
        !first
            .events()
            .iter()
            .any(|event| matches!(event.payload, TaskEventKind::WorkerStarted { .. }))
    );
}

#[test]
fn synchronize_records_one_intent_and_draft_disarms() {
    let old = "a".repeat(40);
    let new = "b".repeat(40);
    let mut ledger = ledger("pr-owner-repo-9");
    record_intents(
        &mut ledger,
        "pr-owner-repo-9",
        "owner/repo",
        9,
        &[ObserverIntent::Arm { sha: old.clone() }],
    )
    .unwrap();
    let cycle = cycle_from_ledger(&ledger);
    let intents = reconcile_snapshot(
        &policy(false),
        &pr(&new),
        &cycle,
        &checks(&new),
        Some(old.as_str()),
        true,
    );
    record_intents(&mut ledger, "pr-owner-repo-9", "owner/repo", 9, &intents).unwrap();
    assert_eq!(
        ledger
            .events()
            .iter()
            .filter(|event| matches!(event.payload, TaskEventKind::ReviewSynchronizeIntent { .. }))
            .count(),
        1
    );
    let mut draft = pr(&old);
    draft.draft = true;
    let after_draft = reconcile_snapshot(
        &policy(false),
        &draft,
        &cycle_from_ledger(&ledger),
        &checks(&old),
        Some(old.as_str()),
        false,
    );
    assert!(
        after_draft
            .iter()
            .any(|intent| matches!(intent, ObserverIntent::ObserveDraft { .. }))
    );
    assert!(
        !after_draft
            .iter()
            .any(|intent| matches!(intent, ObserverIntent::Eligible { .. }))
    );
}

#[test]
fn observer_source_is_read_only_and_config_driven() {
    let src = include_str!("pr_review_observer.rs");
    assert!(src.contains(".get("));
    assert!(!src.contains(".post("));
    assert!(!src.contains(".patch("));
    assert!(!src.contains("check_status"));
    assert!(!src.contains("gh pr checks"));
    let identity = format!("{}Thump", "Forrest");
    assert!(!src.contains(&identity));
    assert!(!src.contains("std::process::Command"));
    assert!(!src.contains("Command::new"));
}
