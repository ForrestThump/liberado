use super::control_plane::*;

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

#[test]
fn ci_pass_or_review_alone_cannot_mark_a_task_ready() {
    let mut ledger = TaskLedger::new(created("task-ready-gate")).expect("ledger");
    ledger
        .append(TaskEvent::new(
            "evt-pr",
            "task-ready-gate",
            TaskEventKind::PullRequestOpened {
                pr_number: 9,
                url: "https://github.com/ForrestThump/liberado/pull/9".into(),
            },
        ))
        .unwrap();
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

    ledger
        .append(TaskEvent::new(
            "evt-review",
            "task-ready-gate",
            TaskEventKind::ReviewApproved {
                reviewer: CONTROLLER_LIBERADO_SHEPHERD.into(),
                round: 2,
            },
        ))
        .unwrap();
    let after_review = ledger.project().unwrap();
    assert_eq!(after_review.review_state, ReviewState::Approved);
    assert!(!after_review.is_pr_ready());

    ledger
        .append(
            TaskEvent::new(
                "evt-ready",
                "task-ready-gate",
                TaskEventKind::ReadyDecided {
                    head_sha: "abc123def456".into(),
                    ci_github_run_id: Some(77),
                    review_round: 2,
                },
            )
            .with_command_id("ready:9:abc123def456"),
        )
        .unwrap();
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
                },
            )
            .with_command_id("repair:5:aaa:1"),
        )
        .unwrap();
    assert_eq!(ledger.project().unwrap().repair_count, 1);
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
fn shepherd_task_id_is_one_safe_component() {
    assert_eq!(
        shepherd_task_id(Some("ForrestThump/liberado"), 42),
        "pr-ForrestThump-liberado-42"
    );
    assert_eq!(shepherd_task_id(None, 7), "pr-7");
}
