use super::control_plane::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn created(task_id: &str) -> TaskEvent {
    TaskEvent::new(
        "evt-created",
        task_id,
        TaskEventKind::TaskCreated {
            objective: "Shepherd one PR".into(),
            acceptance_criteria: vec!["CI and review evidence bound".into()],
            worktree: "worktrees/lease".into(),
            branch: "feat/slice".into(),
            base_ref: "main".into(),
            repo: Some("ForrestThump/liberado".into()),
        },
    )
}

fn append(ledger: &mut TaskLedger, event_id: &str, kind: TaskEventKind) {
    let task_id = ledger.project().unwrap().task_id;
    ledger
        .append(TaskEvent::new(event_id, task_id, kind))
        .unwrap();
}

fn observe_head(ledger: &mut TaskLedger, sha: &str) {
    append(
        ledger,
        &format!("evt-head-{sha}"),
        TaskEventKind::HeadRevisionObserved { sha: sha.into() },
    );
}

fn observe_ci(ledger: &mut TaskLedger, sha: &str, run: u64) {
    append(
        ledger,
        &format!("evt-ci-{sha}-{run}"),
        TaskEventKind::CiObserved {
            github_run_id: Some(run),
            head_sha: Some(sha.into()),
            state: "success".into(),
            failures: Vec::new(),
        },
    );
}

#[test]
fn stale_worker_failure_cannot_block_a_newer_head() {
    let mut ledger = TaskLedger::new(created("task-stale-worker")).unwrap();
    observe_head(&mut ledger, "old-head");
    observe_head(&mut ledger, "new-head");
    append(
        &mut ledger,
        "evt-old-run-finished",
        TaskEventKind::WorkerFinished {
            run_id: "run-old".into(),
            status: WorkerStatus::Failed,
            external_session_id: None,
            blocking_issue: Some("old repair failed".into()),
            revision: Some("old-head".into()),
        },
    );

    let record = ledger.project().unwrap();
    assert_eq!(record.head_revision.as_deref(), Some("new-head"));
    assert_eq!(record.disposition, TaskDisposition::Open);
}

#[test]
fn stale_worker_finish_cannot_clear_a_newer_active_run() {
    let mut ledger = TaskLedger::new(created("task-stale-worker-run")).unwrap();
    observe_head(&mut ledger, "old-head");
    append(
        &mut ledger,
        "evt-old-run-started",
        TaskEventKind::WorkerStarted {
            run_id: "run-old".into(),
            worker_id: "worker-old".into(),
            resumed_session_id: None,
        },
    );
    observe_head(&mut ledger, "new-head");
    append(
        &mut ledger,
        "evt-new-run-started",
        TaskEventKind::WorkerStarted {
            run_id: "run-new".into(),
            worker_id: "worker-new".into(),
            resumed_session_id: None,
        },
    );
    append(
        &mut ledger,
        "evt-old-run-finished",
        TaskEventKind::WorkerFinished {
            run_id: "run-old".into(),
            status: WorkerStatus::Failed,
            external_session_id: Some("session-old".into()),
            blocking_issue: Some("old repair failed".into()),
            revision: Some("old-head".into()),
        },
    );

    let record = ledger.project().unwrap();
    assert_eq!(record.active_run_id.as_deref(), Some("run-new"));
    assert_eq!(record.external_session_id, None);
    assert_eq!(record.status, TaskStatus::Running);
    assert_eq!(record.disposition, TaskDisposition::Open);
}

#[test]
fn current_head_worker_finish_must_match_the_active_run() {
    let mut ledger = TaskLedger::new(created("task-wrong-worker-run")).unwrap();
    observe_head(&mut ledger, "current-head");
    append(
        &mut ledger,
        "evt-new-run-started",
        TaskEventKind::WorkerStarted {
            run_id: "run-new".into(),
            worker_id: "worker-new".into(),
            resumed_session_id: None,
        },
    );
    append(
        &mut ledger,
        "evt-old-run-finished",
        TaskEventKind::WorkerFinished {
            run_id: "run-old".into(),
            status: WorkerStatus::Failed,
            external_session_id: Some("session-old".into()),
            blocking_issue: Some("superseded run failed".into()),
            revision: Some("current-head".into()),
        },
    );

    let record = ledger.project().unwrap();
    assert_eq!(record.active_run_id.as_deref(), Some("run-new"));
    assert_eq!(record.external_session_id, None);
    assert_eq!(record.status, TaskStatus::Running);
    assert_eq!(record.disposition, TaskDisposition::Open);
}

#[test]
fn stale_worker_finish_cannot_apply_after_the_active_run_clears() {
    let mut ledger = TaskLedger::new(created("task-cleared-worker-run")).unwrap();
    observe_head(&mut ledger, "current-head");
    append(
        &mut ledger,
        "evt-run-started",
        TaskEventKind::WorkerStarted {
            run_id: "run-current".into(),
            worker_id: "worker-current".into(),
            resumed_session_id: None,
        },
    );
    append(
        &mut ledger,
        "evt-run-finished",
        TaskEventKind::WorkerFinished {
            run_id: "run-current".into(),
            status: WorkerStatus::Completed,
            external_session_id: Some("session-current".into()),
            blocking_issue: None,
            revision: Some("current-head".into()),
        },
    );
    append(
        &mut ledger,
        "evt-stale-run-finished",
        TaskEventKind::WorkerFinished {
            run_id: "run-stale".into(),
            status: WorkerStatus::Failed,
            external_session_id: Some("session-stale".into()),
            blocking_issue: Some("stale repair failed".into()),
            revision: Some("current-head".into()),
        },
    );

    let record = ledger.project().unwrap();
    assert_eq!(record.active_run_id, None);
    assert_eq!(
        record.external_session_id.as_deref(),
        Some("session-current")
    );
    assert_eq!(record.disposition, TaskDisposition::Open);
    assert_eq!(record.current_diagnosis, None);
}

#[test]
fn stale_successful_repair_cannot_change_a_newer_head() {
    let mut ledger = TaskLedger::new(created("task-stale-success")).unwrap();
    observe_head(&mut ledger, "old-head");
    observe_head(&mut ledger, "new-head");
    append(
        &mut ledger,
        "evt-old-run-finished",
        TaskEventKind::WorkerFinished {
            run_id: "run-old".into(),
            status: WorkerStatus::Completed,
            external_session_id: None,
            blocking_issue: None,
            revision: Some("old-head".into()),
        },
    );
    append(
        &mut ledger,
        "evt-old-repair-commit",
        TaskEventKind::CommitProduced {
            commit_sha: "stale-commit".into(),
            message: "repair old head".into(),
            files_changed: vec!["src/stale.rs".into()],
            revision: Some("old-head".into()),
            run_id: Some("run-old".into()),
        },
    );

    let record = ledger.project().unwrap();
    assert_eq!(record.head_revision.as_deref(), Some("new-head"));
    assert_eq!(record.disposition, TaskDisposition::Open);
    assert!(record.commits.is_empty());
    assert!(record.files_changed.is_empty());
}

#[test]
fn stale_commit_produced_cannot_pollute_active_run() {
    let mut ledger = TaskLedger::new(created("task-stale-commit")).unwrap();
    observe_head(&mut ledger, "old-head");
    observe_head(&mut ledger, "current-head");
    append(
        &mut ledger,
        "evt-run-started",
        TaskEventKind::WorkerStarted {
            run_id: "run-current".into(),
            worker_id: "worker-current".into(),
            resumed_session_id: None,
        },
    );
    append(
        &mut ledger,
        "evt-stale-commit",
        TaskEventKind::CommitProduced {
            commit_sha: "stale-commit-sha".into(),
            message: "repair on current head by a superseded worker".into(),
            files_changed: vec!["src/stale.rs".into()],
            revision: Some("current-head".into()),
            run_id: Some("run-superseded".into()),
        },
    );

    let record = ledger.project().unwrap();
    assert_eq!(record.active_run_id.as_deref(), Some("run-current"));
    assert_eq!(record.head_revision.as_deref(), Some("current-head"));
    assert!(record.commits.is_empty(), "a stale commit produced must not pollute commits");
    assert!(record.files_changed.is_empty(), "a stale commit produced must not pollute files_changed");
}

fn approve_review(ledger: &mut TaskLedger, round: usize) {
    append(
        ledger,
        &format!("evt-review-{round}"),
        TaskEventKind::ReviewApproved {
            reviewer: CONTROLLER_LIBERADO_SHEPHERD.into(),
            round,
        },
    );
}

fn decide_ready(ledger: &mut TaskLedger, sha: &str, run: u64, round: u32) {
    ledger
        .append(
            TaskEvent::new(
                format!("evt-ready-{sha}"),
                &ledger.project().unwrap().task_id,
                TaskEventKind::ReadyDecided {
                    head_sha: sha.into(),
                    ci_github_run_id: Some(run),
                    review_round: round,
                },
            )
            .with_command_id(format!("ready:9:{sha}")),
        )
        .unwrap();
}

fn open_pr(ledger: &mut TaskLedger) {
    append(
        ledger,
        "evt-pr",
        TaskEventKind::PullRequestOpened {
            pr_number: 9,
            url: "https://github.com/ForrestThump/liberado/pull/9".into(),
        },
    );
}

#[test]
fn ci_pass_or_review_alone_cannot_mark_a_task_ready() {
    let mut ledger = TaskLedger::new(created("task-ready-gate")).expect("ledger");
    open_pr(&mut ledger);
    ledger
        .append(TaskEvent::new(
            "evt-ci",
            "task-ready-gate",
            TaskEventKind::CiPassed,
        ))
        .unwrap();

    let after_ci = ledger.project().unwrap();
    assert_eq!(after_ci.ci_state, CiState::Passed);
    assert_eq!(after_ci.status, TaskStatus::NeedsReview);
    assert_eq!(after_ci.disposition, TaskDisposition::Open);
    assert!(!after_ci.is_pr_ready());

    approve_review(&mut ledger, 2);
    let after_review = ledger.project().unwrap();
    assert_eq!(after_review.review_state, ReviewState::Approved);
    assert!(!after_review.is_pr_ready());

    observe_head(&mut ledger, "abc123def456");
    observe_ci(&mut ledger, "abc123def456", 77);
    append(
        &mut ledger,
        "evt-review-bound",
        TaskEventKind::ReviewApproved {
            reviewer: CONTROLLER_LIBERADO_SHEPHERD.into(),
            round: 2,
        },
    );
    decide_ready(&mut ledger, "abc123def456", 77, 2);
    let ready = ledger.project().unwrap();
    assert!(ready.is_pr_ready());
    assert_eq!(ready.status, TaskStatus::Completed);
    assert_eq!(ready.disposition, TaskDisposition::Ready);
    assert_eq!(ready.head_revision.as_deref(), Some("abc123def456"));
    assert_eq!(
        ready.ready_evidence.as_ref().unwrap().ci_github_run_id,
        Some(77)
    );
}

#[test]
fn ready_decided_alone_cannot_mark_a_task_ready() {
    let mut ledger = TaskLedger::new(created("task-ready-only")).expect("ledger");
    open_pr(&mut ledger);
    decide_ready(&mut ledger, "abc123def456", 77, 2);
    let record = ledger.project().unwrap();
    assert_eq!(record.ci_state, CiState::Unknown);
    assert_eq!(record.review_state, ReviewState::None);
    assert_projected_not_complete(&record);
}

#[test]
fn ready_decided_without_evidence_does_not_project_ready_or_completed() {
    let mut ledger = TaskLedger::new(created("task-ready-proj")).expect("ledger");
    open_pr(&mut ledger);
    let before = ledger.project().unwrap();
    decide_ready(&mut ledger, "abc123def456", 77, 2);
    let record = ledger.project().unwrap();
    assert_eq!(record.disposition, before.disposition);
    assert_eq!(record.status, before.status);
    assert_ne!(record.disposition, TaskDisposition::Ready);
    assert_ne!(record.status, TaskStatus::Completed);
    assert_projected_not_complete(&record);
}

#[test]
fn ready_without_ci_cannot_mark_a_task_ready() {
    let mut ledger = TaskLedger::new(created("task-ready-no-ci")).expect("ledger");
    open_pr(&mut ledger);
    observe_head(&mut ledger, "abc123def456");
    approve_review(&mut ledger, 2);
    decide_ready(&mut ledger, "abc123def456", 77, 2);
    assert_projected_not_complete(&ledger.project().unwrap());
}

#[test]
fn ready_without_review_cannot_mark_a_task_ready() {
    let mut ledger = TaskLedger::new(created("task-ready-no-review")).expect("ledger");
    open_pr(&mut ledger);
    observe_head(&mut ledger, "abc123def456");
    observe_ci(&mut ledger, "abc123def456", 77);
    decide_ready(&mut ledger, "abc123def456", 77, 2);
    assert_projected_not_complete(&ledger.project().unwrap());
}

#[test]
fn ready_with_mismatched_head_cannot_mark_a_task_ready() {
    let mut ledger = TaskLedger::new(created("task-ready-head")).expect("ledger");
    open_pr(&mut ledger);
    observe_head(&mut ledger, "aaaaaaaaaaaa");
    observe_ci(&mut ledger, "aaaaaaaaaaaa", 77);
    approve_review(&mut ledger, 2);
    decide_ready(&mut ledger, "bbbbbbbbbbbb", 77, 2);
    assert_projected_not_complete(&ledger.project().unwrap());
}

#[test]
fn ready_with_mismatched_run_cannot_mark_a_task_ready() {
    let mut ledger = TaskLedger::new(created("task-ready-run")).expect("ledger");
    open_pr(&mut ledger);
    observe_head(&mut ledger, "abc123def456");
    observe_ci(&mut ledger, "abc123def456", 77);
    approve_review(&mut ledger, 2);
    decide_ready(&mut ledger, "abc123def456", 88, 2);
    assert_projected_not_complete(&ledger.project().unwrap());
}

#[test]
fn ready_with_stale_review_cannot_mark_a_task_ready() {
    let mut ledger = TaskLedger::new(created("task-ready-stale")).expect("ledger");
    open_pr(&mut ledger);
    observe_head(&mut ledger, "oldheadoldhead");
    approve_review(&mut ledger, 1);
    observe_head(&mut ledger, "newheadnewhead");
    observe_ci(&mut ledger, "newheadnewhead", 90);
    decide_ready(&mut ledger, "newheadnewhead", 90, 1);
    let record = ledger.project().unwrap();
    assert_eq!(
        record.review_evidence_sha.as_deref(),
        Some("oldheadoldhead")
    );
    assert_projected_not_complete(&record);
}

fn assert_projected_not_complete(record: &TaskRecord) {
    assert_ne!(
        record.disposition,
        TaskDisposition::Ready,
        "ReadyDecided must not project Ready without bound CI and review evidence"
    );
    assert_ne!(
        record.status,
        TaskStatus::Completed,
        "ReadyDecided must not project Completed without bound CI and review evidence"
    );
    assert!(!record.is_pr_ready());
}

#[test]
fn truncated_jsonl_recovers_complete_events_and_accepts_a_later_append() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut ledger = TaskLedger::create_in(temp.path(), created("task-trunc")).expect("create");
    ledger
        .append(
            TaskEvent::new(
                "evt-head",
                "task-trunc",
                TaskEventKind::HeadRevisionObserved {
                    sha: "deadbeef".into(),
                },
            )
            .with_command_id("head:1:deadbeef"),
        )
        .unwrap();
    drop(ledger);

    let path = temp.path().join("task-trunc/ledger.jsonl");
    let mut bytes = std::fs::read(&path).expect("read");
    bytes.extend_from_slice(b"{\"event_id\":\"evt-cut\",\"task_id\":\"task-trunc\"");
    std::fs::write(&path, bytes).expect("truncate");

    let mut restored = TaskLedger::load_from_path(&path).expect("recover");
    assert_eq!(restored.events().len(), 2);
    assert_eq!(
        restored.project().unwrap().head_revision.as_deref(),
        Some("deadbeef")
    );

    let written = restored
        .record(
            TaskEvent::new(
                "evt-ci",
                "task-trunc",
                TaskEventKind::CiObserved {
                    github_run_id: Some(3),
                    head_sha: Some("deadbeef".into()),
                    state: "success".into(),
                    failures: Vec::new(),
                },
            )
            .with_command_id("ci:1:deadbeef:3:success"),
        )
        .expect("append after recovery");
    assert!(written);
    assert_eq!(restored.events().len(), 3);
    assert_eq!(restored.project().unwrap().ci_state, CiState::Passed);
}

#[test]
fn truncated_jsonl_recovers_mid_utf8_and_accepts_a_later_append() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut ledger = TaskLedger::create_in(temp.path(), created("task-utf8")).expect("create");
    ledger
        .append(
            TaskEvent::new(
                "evt-head",
                "task-utf8",
                TaskEventKind::HeadRevisionObserved {
                    sha: "cafebeef".into(),
                },
            )
            .with_command_id("head:1:cafebeef"),
        )
        .unwrap();
    drop(ledger);

    let path = temp.path().join("task-utf8/ledger.jsonl");
    let mut bytes = std::fs::read(&path).expect("read");
    bytes.extend_from_slice(
        br#"{"event_id":"evt-cut","task_id":"task-utf8","payload":{"type":"blocked_decided","reason":"caf"#,
    );
    bytes.push(0xC3);
    std::fs::write(&path, bytes).expect("mid-utf8 tail");

    let mut restored = TaskLedger::load_from_path(&path).expect("recover mid-utf8");
    assert_eq!(restored.events().len(), 2);
    let written = restored
        .record(
            TaskEvent::new(
                "evt-ci",
                "task-utf8",
                TaskEventKind::CiObserved {
                    github_run_id: Some(4),
                    head_sha: Some("cafebeef".into()),
                    state: "success".into(),
                    failures: Vec::new(),
                },
            )
            .with_command_id("ci:1:cafebeef:4:success"),
        )
        .expect("append after utf8 recovery");
    assert!(written);
    assert_eq!(restored.events().len(), 3);
}

#[test]
fn duplicate_command_ids_do_not_write_a_second_event() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut ledger = TaskLedger::create_in(temp.path(), created("task-idemp")).expect("create");
    let first = ledger
        .record(
            TaskEvent::new(
                "evt-obs-1",
                "task-idemp",
                TaskEventKind::CiObserved {
                    github_run_id: Some(11),
                    head_sha: Some("aaa".into()),
                    state: "failure".into(),
                    failures: vec!["job|test".into()],
                },
            )
            .with_command_id("ci:5:aaa:11:failure"),
        )
        .unwrap();
    let second = ledger
        .record(
            TaskEvent::new(
                "evt-obs-2",
                "task-idemp",
                TaskEventKind::CiObserved {
                    github_run_id: Some(11),
                    head_sha: Some("aaa".into()),
                    state: "failure".into(),
                    failures: vec!["job|test".into()],
                },
            )
            .with_command_id("ci:5:aaa:11:failure"),
        )
        .unwrap();
    assert!(first);
    assert!(!second);
    assert_eq!(ledger.events().len(), 2);
    assert_eq!(ledger.project().unwrap().repair_count, 0);

    ledger
        .append(
            TaskEvent::new(
                "evt-repair",
                "task-idemp",
                TaskEventKind::RepairRequested {
                    goal_id: Some("goal-1".into()),
                    reason: "new CI failures".into(),
                    cause_event_id: Some("evt-ci".into()),
                },
            )
            .with_command_id("repair:5:aaa:1"),
        )
        .unwrap();
    ledger
        .append(
            TaskEvent::new(
                "evt-repair-dup",
                "task-idemp",
                TaskEventKind::RepairRequested {
                    goal_id: Some("goal-1".into()),
                    reason: "new CI failures".into(),
                    cause_event_id: Some("evt-ci".into()),
                },
            )
            .with_command_id("repair:5:aaa:1"),
        )
        .unwrap();
    assert_eq!(ledger.project().unwrap().repair_count, 1);
}

#[test]
fn duplicate_command_ids_are_rejected_across_independently_loaded_writers() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let created_ledger = TaskLedger::create_in(temp.path(), created("task-race")).expect("create");
    let path = temp.path().join("task-race/ledger.jsonl");
    drop(created_ledger);

    let mut first = TaskLedger::load_from_path(&path).expect("writer one");
    let mut second = TaskLedger::load_from_path(&path).expect("writer two");
    let wrote_first = first
        .record(
            TaskEvent::new(
                "evt-obs-a",
                "task-race",
                TaskEventKind::CiObserved {
                    github_run_id: Some(11),
                    head_sha: Some("aaa".into()),
                    state: "failure".into(),
                    failures: vec!["job|test".into()],
                },
            )
            .with_command_id("ci:5:aaa:11:failure"),
        )
        .unwrap();
    let wrote_second = second
        .record(
            TaskEvent::new(
                "evt-obs-b",
                "task-race",
                TaskEventKind::CiObserved {
                    github_run_id: Some(11),
                    head_sha: Some("aaa".into()),
                    state: "failure".into(),
                    failures: vec!["job|test".into()],
                },
            )
            .with_command_id("ci:5:aaa:11:failure"),
        )
        .unwrap();
    assert!(wrote_first);
    assert!(
        !wrote_second,
        "the second writer must reload under the lock and skip the same command_id"
    );
    let reloaded = TaskLedger::load_from_path(&path).expect("reload");
    let copies = reloaded
        .events()
        .iter()
        .filter(|event| event.command_id.as_deref() == Some("ci:5:aaa:11:failure"))
        .count();
    assert_eq!(copies, 1);
    assert_eq!(reloaded.events().len(), 2);
}

#[test]
fn one_controller_lease_per_task() {
    let mut ledger = TaskLedger::new(created("task-lease")).expect("ledger");
    ledger
        .append(
            TaskEvent::new(
                "evt-lease",
                "task-lease",
                TaskEventKind::ControllerLeaseClaimed {
                    controller: CONTROLLER_LIBERADO_SHEPHERD.into(),
                },
            )
            .with_command_id("lease:1:liberado-shepherd"),
        )
        .unwrap();
    let err = ledger
        .append(
            TaskEvent::new(
                "evt-lease-other",
                "task-lease",
                TaskEventKind::ControllerLeaseClaimed {
                    controller: CONTROLLER_GROK_BOT.into(),
                },
            )
            .with_command_id("lease:1:grok-bot"),
        )
        .expect_err("second controller must not take the lease");
    assert!(matches!(
        err,
        ControlPlaneError::ControllerLeaseConflict { .. }
    ));
}

#[test]
fn durable_root_is_repository_scoped_not_the_worktree_leaf() {
    let repo = tempfile::TempDir::new().expect("repo");
    let worktree = repo.path().join("worktrees/worker-1");
    std::fs::create_dir_all(&worktree).unwrap();
    let root = durable_tasks_root(repo.path());
    let ledger = TaskLedger::create_in(
        &root,
        TaskEvent::new(
            "evt-created",
            "task-durable-home",
            TaskEventKind::TaskCreated {
                objective: "keep the ledger".into(),
                acceptance_criteria: Vec::new(),
                worktree: worktree.to_string_lossy().into_owned(),
                branch: "feat/x".into(),
                base_ref: "main".into(),
                repo: None,
            },
        ),
    )
    .expect("create at repo root");
    assert!(root.join("task-durable-home/ledger.jsonl").is_file());
    assert!(!worktree.join(".liberado").exists());
    let record = ledger.project().unwrap();
    assert_eq!(record.worktree, worktree.to_string_lossy());
}

#[test]
fn linked_worktree_resolves_ledger_to_the_main_repository() {
    let temp = tempfile::TempDir::new().expect("temp");
    let main = temp.path().join("main");
    let leaf = temp.path().join("leaf");
    std::fs::create_dir_all(&main).unwrap();
    init_git_repo(&main);
    git(
        &main,
        &["worktree", "add", "--detach", leaf.to_str().unwrap()],
    );

    let resolved = tasks_root_from_worktree(&leaf);
    let expected = durable_tasks_root(canonicalize_dir(&main));
    assert_eq!(
        canonicalize_dir(&resolved),
        canonicalize_dir(&expected),
        "a linked worktree must store ledgers on the main repository"
    );

    TaskLedger::create_in(&resolved, created("task-from-leaf")).expect("create via worktree root");
    assert!(
        expected.join("task-from-leaf/ledger.jsonl").is_file(),
        "ledger must exist on the main repository durable root"
    );
    assert!(
        !leaf.join(".liberado").exists(),
        "a disposable worktree must not be the sole ledger home"
    );
}

#[test]
fn shepherd_task_id_is_one_safe_component() {
    assert_eq!(
        shepherd_task_id(Some("ForrestThump/liberado"), 42),
        "pr-ForrestThump-liberado-42"
    );
    assert_eq!(shepherd_task_id(None, 7), "pr-7");
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn init_git_repo(dir: &Path) {
    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Liberado Test"]);
    git(dir, &["config", "user.email", "test@liberado.local"]);
    std::fs::write(dir.join("README.md"), "ledger home\n").unwrap();
    git(dir, &["add", "README.md"]);
    git(dir, &["commit", "-m", "init"]);
}

fn canonicalize_dir(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
