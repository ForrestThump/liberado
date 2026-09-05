use super::control_plane::*;
use crate::CoderTuning;

fn sample_created_event(task_id: &str) -> TaskEvent {
    TaskEvent::new(
        "evt-001",
        task_id,
        TaskEventKind::TaskCreated {
            objective: "Add directory enumeration in TurboVault".into(),
            acceptance_criteria: vec![
                "Return relative paths".into(),
                "Ignore hidden paths by default".into(),
            ],
            worktree: "worktrees/task-1".into(),
            branch: "feat/turbovault-enum".into(),
            base_ref: "main".into(),
            repo: Some("ForrestThump/liberado".into()),
        },
    )
}

#[test]
fn ledger_initialization_and_projection() {
    let initial = sample_created_event("task-100");
    let ledger = TaskLedger::new(initial).expect("ledger creation");
    let record = ledger.project().expect("projection");

    assert_eq!(record.task_id, "task-100");
    assert_eq!(record.status, TaskStatus::Created);
    assert_eq!(record.objective, "Add directory enumeration in TurboVault");
    assert_eq!(record.acceptance_criteria.len(), 2);
    assert_eq!(record.branch, "feat/turbovault-enum");
    assert_eq!(record.base_ref, "main");
    assert_eq!(record.worktree, "worktrees/task-1");
    assert_eq!(record.repo.as_deref(), Some("ForrestThump/liberado"));
    assert!(record.commits.is_empty());
    assert!(record.files_changed.is_empty());
    assert!(record.failures.is_empty());
}

#[test]
fn ledger_rejects_non_task_created_as_first_event() {
    let event = TaskEvent::new(
        "evt-002",
        "task-100",
        TaskEventKind::CommitProduced {
            commit_sha: "abc1234".into(),
            message: "initial commit".into(),
            files_changed: vec!["src/lib.rs".into()],
        },
    );
    let result = TaskLedger::new(event);
    assert!(matches!(
        result,
        Err(ControlPlaneError::InvalidInitialEvent(_))
    ));
}

#[test]
fn ledger_rejects_mismatched_task_id() {
    let initial = sample_created_event("task-100");
    let mut ledger = TaskLedger::new(initial).expect("ledger");

    let mismatched = TaskEvent::new(
        "evt-002",
        "task-999",
        TaskEventKind::WorkerStarted {
            run_id: "run-1".into(),
            worker_id: "codex".into(),
            resumed_session_id: None,
        },
    );

    let err = ledger
        .append(mismatched)
        .expect_err("should reject mismatch");
    assert!(matches!(err, ControlPlaneError::TaskIdMismatch { .. }));
}

#[test]
fn ledger_event_lifecycle_projection() {
    let initial = sample_created_event("task-100");
    let mut ledger = TaskLedger::new(initial).expect("ledger");

    // 1. Worker started
    ledger
        .append(TaskEvent::new(
            "evt-002",
            "task-100",
            TaskEventKind::WorkerStarted {
                run_id: "run-1".into(),
                worker_id: "codex-cli".into(),
                resumed_session_id: Some("session-xyz".into()),
            },
        ))
        .unwrap();

    let rec = ledger.project().unwrap();
    assert_eq!(rec.status, TaskStatus::Running);
    assert_eq!(rec.prior_worker.as_deref(), Some("codex-cli"));
    assert_eq!(rec.external_session_id.as_deref(), Some("session-xyz"));

    // 2. Commit produced
    ledger
        .append(TaskEvent::new(
            "evt-003",
            "task-100",
            TaskEventKind::CommitProduced {
                commit_sha: "c0ffee1".into(),
                message: "Add directory walk".into(),
                files_changed: vec!["crates/vault/src/lib.rs".into()],
            },
        ))
        .unwrap();

    let rec = ledger.project().unwrap();
    assert_eq!(rec.commits, vec!["c0ffee1".to_string()]);
    assert_eq!(
        rec.files_changed,
        vec!["crates/vault/src/lib.rs".to_string()]
    );

    // 3. PR opened
    ledger
        .append(TaskEvent::new(
            "evt-004",
            "task-100",
            TaskEventKind::PullRequestOpened {
                pr_number: 42,
                url: "https://github.com/ForrestThump/liberado/pull/42".into(),
            },
        ))
        .unwrap();

    let rec = ledger.project().unwrap();
    assert_eq!(rec.status, TaskStatus::NeedsReview);
    assert_eq!(rec.pull_request_number, Some(42));

    // 4. CI Failed
    ledger
        .append(TaskEvent::new(
            "evt-005",
            "task-100",
            TaskEventKind::CiFailed {
                run_id: Some("ci-run-99".into()),
                failures: vec!["test_hidden_dir_ignored".into()],
                failure_log_excerpt: Some("assertion failed: !res.contains(\".git\")".into()),
            },
        ))
        .unwrap();

    let rec = ledger.project().unwrap();
    assert_eq!(rec.status, TaskStatus::Repairing);
    assert_eq!(rec.failures, vec!["test_hidden_dir_ignored".to_string()]);
    assert_eq!(
        rec.latest_failure_excerpt.as_deref(),
        Some("assertion failed: !res.contains(\".git\")")
    );

    // 5. Worker resumed
    ledger
        .append(TaskEvent::new(
            "evt-006",
            "task-100",
            TaskEventKind::WorkerResumed {
                run_id: "run-2".into(),
                worker_id: "liberado-native".into(),
                reason: "Repair CI failure".into(),
            },
        ))
        .unwrap();

    let rec = ledger.project().unwrap();
    assert_eq!(rec.status, TaskStatus::Running);
    assert_eq!(rec.prior_worker.as_deref(), Some("liberado-native"));

    // 6. Tests passed & CI Passed
    ledger
        .append(TaskEvent::new(
            "evt-007",
            "task-100",
            TaskEventKind::TestsPassed { tests_run: 5 },
        ))
        .unwrap();
    ledger
        .append(TaskEvent::new(
            "evt-008",
            "task-100",
            TaskEventKind::CiPassed,
        ))
        .unwrap();

    let rec = ledger.project().unwrap();
    assert_eq!(rec.status, TaskStatus::Completed);
    assert!(rec.failures.is_empty());
    assert!(rec.latest_failure_excerpt.is_none());
}

#[test]
fn ledger_serialization_round_trip() {
    let initial = sample_created_event("task-200");
    let mut original = TaskLedger::new(initial).expect("ledger");
    original
        .append(TaskEvent::new(
            "evt-002",
            "task-200",
            TaskEventKind::WorkerStarted {
                run_id: "run-1".into(),
                worker_id: "codex".into(),
                resumed_session_id: None,
            },
        ))
        .unwrap();
    original
        .append(TaskEvent::new(
            "evt-003",
            "task-200",
            TaskEventKind::CommitProduced {
                commit_sha: "fedcba9".into(),
                message: "feat: implemented".into(),
                files_changed: vec!["Cargo.toml".into()],
            },
        ))
        .unwrap();

    let mut buffer = Vec::new();
    original.write_to_writer(&mut buffer).expect("write JSONL");

    let restored = TaskLedger::load_from_reader(&buffer[..]).expect("read JSONL");
    assert_eq!(original.events().len(), restored.events().len());
    assert_eq!(original.project().unwrap(), restored.project().unwrap());
}

#[test]
fn disk_ledger_appends_one_flushed_event_and_restores_projection() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let initial = sample_created_event("task-durable");
    let mut ledger = TaskLedger::create_in(temp.path(), initial).expect("create persistent ledger");
    ledger
        .append(TaskEvent::new(
            "evt-worker",
            "task-durable",
            TaskEventKind::WorkerStarted {
                run_id: "run-1".into(),
                worker_id: "opencode".into(),
                resumed_session_id: None,
            },
        ))
        .expect("append event");

    let ledger_path = temp.path().join("task-durable/ledger.jsonl");
    let persisted = std::fs::read_to_string(&ledger_path).expect("read ledger");
    assert_eq!(persisted.lines().count(), 2);
    let restored = TaskLedger::load_from_path(&ledger_path).expect("restore ledger");
    assert_eq!(restored.project().unwrap().status, TaskStatus::Running);
    assert!(temp.path().join("task-durable/task.json").is_file());
}

#[test]
fn disk_ledger_create_in_reloads_existing_for_same_task_id() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let initial = sample_created_event("task-retry");
    let mut ledger = TaskLedger::create_in(temp.path(), initial).expect("create persistent ledger");
    ledger
        .append(TaskEvent::new(
            "evt-worker",
            "task-retry",
            TaskEventKind::WorkerStarted {
                run_id: "run-1".into(),
                worker_id: "opencode".into(),
                resumed_session_id: None,
            },
        ))
        .expect("append event");
    assert_eq!(ledger.events().len(), 2);

    let retry_event = TaskEvent::new(
        "evt-retry-001",
        "task-retry",
        TaskEventKind::TaskCreated {
            objective: "Retry objective".into(),
            acceptance_criteria: vec![],
            worktree: "worktrees/task-retry".into(),
            branch: "feat/retry".into(),
            base_ref: "main".into(),
            repo: None,
        },
    );
    let mut reloaded =
        TaskLedger::create_in(temp.path(), retry_event).expect("reload existing persistent ledger");

    assert_eq!(reloaded.events().len(), 2);
    assert_eq!(reloaded.events()[0].event_id, "evt-001");
    assert_eq!(reloaded.events()[1].event_id, "evt-worker");

    reloaded
        .append(TaskEvent::new(
            "evt-completed",
            "task-retry",
            TaskEventKind::WorkerFinished {
                run_id: "run-1".into(),
                status: WorkerStatus::Completed,
                external_session_id: None,
                blocking_issue: None,
            },
        ))
        .expect("append after reload");

    assert_eq!(reloaded.events().len(), 3);
    let ledger_path = temp.path().join("task-retry/ledger.jsonl");
    let persisted = std::fs::read_to_string(&ledger_path).expect("read ledger");
    assert_eq!(persisted.lines().count(), 3);

    // Mismatched task_id fails closed
    let mismatch_dir = temp.path().join("task-mismatch");
    std::fs::create_dir_all(&mismatch_dir).expect("mismatch dir");
    std::fs::copy(&ledger_path, mismatch_dir.join("ledger.jsonl")).expect("copy ledger");
    let mismatch_err = TaskLedger::create_in(temp.path(), sample_created_event("task-mismatch"));
    assert!(matches!(
        mismatch_err,
        Err(ControlPlaneError::TaskIdMismatch {
            event_task_id,
            ledger_task_id,
        }) if event_task_id == "task-mismatch" && ledger_task_id == "task-retry"
    ));
}

#[test]
fn disk_ledger_rejects_path_traversal_task_ids() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let event = sample_created_event("../outside");
    assert!(matches!(
        TaskLedger::create_in(temp.path(), event),
        Err(ControlPlaneError::InvalidTaskId(_))
    ));
}

#[test]
fn continuation_context_builder_generates_markdown() {
    let initial = sample_created_event("task-341");
    let mut ledger = TaskLedger::new(initial).expect("ledger");
    ledger
        .append(TaskEvent::new(
            "evt-002",
            "task-341",
            TaskEventKind::CommitProduced {
                commit_sha: "a1b2c3d".into(),
                message: "Initial walker".into(),
                files_changed: vec!["crates/vault/src/lib.rs".into()],
            },
        ))
        .unwrap();
    ledger
        .append(TaskEvent::new(
            "evt-003",
            "task-341",
            TaskEventKind::CiFailed {
                run_id: Some("run-88".into()),
                failures: vec!["turbovault::tests::test_hidden_dir_ignored".into()],
                failure_log_excerpt: Some(
                    "test tests::test_hidden_dir_ignored ... FAILED\nassertion failed".into(),
                ),
            },
        ))
        .unwrap();

    let record = ledger.project().unwrap();
    let prompt = ContinuationContextBuilder::build(&record);

    assert!(prompt.contains("You are continuing work on task `task-341`."));
    assert!(prompt.contains("## Objective\nAdd directory enumeration in TurboVault"));
    assert!(prompt.contains("- Return relative paths"));
    assert!(prompt.contains("- Branch: `feat/turbovault-enum`"));
    assert!(prompt.contains("- `a1b2c3d`"));
    assert!(prompt.contains("turbovault::tests::test_hidden_dir_ignored"));
    assert!(prompt.contains("test tests::test_hidden_dir_ignored ... FAILED"));
    assert!(prompt.contains("## Instructions"));
}

struct MockWorker {
    received_requests: std::sync::Mutex<Vec<WorkerRunRequest>>,
    received_resumes: std::sync::Mutex<Vec<(RunHandle, TaskEvent)>>,
    return_commits: std::sync::Mutex<Vec<Vec<String>>>,
    session_id: Option<String>,
}

impl MockWorker {
    fn new(return_commits: Vec<Vec<String>>) -> Self {
        Self {
            received_requests: std::sync::Mutex::new(Vec::new()),
            received_resumes: std::sync::Mutex::new(Vec::new()),
            return_commits: std::sync::Mutex::new(return_commits),
            session_id: Some("mock-session-123".into()),
        }
    }

    fn without_session(return_commits: Vec<Vec<String>>) -> Self {
        Self {
            session_id: None,
            ..Self::new(return_commits)
        }
    }
}

impl WorkerPort for MockWorker {
    fn id(&self) -> &str {
        "mock-worker"
    }

    fn start(&self, req: &WorkerRunRequest) -> Result<RunHandle, ControlPlaneError> {
        let run_number = {
            let mut requests = self.received_requests.lock().unwrap();
            requests.push(req.clone());
            requests.len()
        };
        let mut handle = RunHandle::new(
            format!("mock-run-{run_number}"),
            self.id(),
            &req.task_id,
            &req.worktree,
        );
        if let Some(session_id) = &self.session_id {
            handle = handle.with_session_id(session_id.clone());
        }
        Ok(handle)
    }

    fn resume(
        &self,
        handle: &RunHandle,
        event: &TaskEvent,
    ) -> Result<RunHandle, ControlPlaneError> {
        self.received_resumes
            .lock()
            .unwrap()
            .push((handle.clone(), event.clone()));
        let mut resumed =
            RunHandle::new("mock-run-2", self.id(), &handle.task_id, &handle.worktree);
        if let Some(session_id) = &self.session_id {
            resumed = resumed.with_session_id(session_id.clone());
        }
        Ok(resumed)
    }

    fn status(&self, _handle: &RunHandle) -> Result<WorkerStatus, ControlPlaneError> {
        Ok(WorkerStatus::Completed)
    }

    fn cancel(&self, _handle: &RunHandle) -> Result<(), ControlPlaneError> {
        Ok(())
    }

    fn collect(&self, _handle: &RunHandle) -> Result<WorkerRunResult, ControlPlaneError> {
        let mut queue = self.return_commits.lock().unwrap();
        let commits = if !queue.is_empty() {
            queue.remove(0)
        } else {
            Vec::new()
        };
        Ok(WorkerRunResult {
            status: WorkerStatus::Completed,
            summary: "Completed work successfully".into(),
            commits,
            files_changed: vec!["crates/test/src/lib.rs".into()],
            tests_run: 2,
            tests_passed: 2,
            blocking_issue: None,
            recommended_next_action: None,
            external_session_id: self.session_id.clone(),
        })
    }
}

#[test]
fn supervisor_dispatch_task_initializes_ledger_and_records_run() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mock = std::sync::Arc::new(MockWorker::new(vec![vec!["commit-aaa".into()]]));
    let supervisor = ControlPlaneSupervisor::new(mock.clone());

    let req = DispatchTaskRequest::new(
        "task-400",
        "Fix query parser regex",
        temp.path().to_string_lossy(),
        "feat/query-parser",
        "main",
    )
    .with_acceptance_criteria(vec!["Pass all parser tests".into()])
    .with_repo("ForrestThump/liberado");

    let (ledger, result) = supervisor.dispatch_task(&req).expect("dispatch");

    assert_eq!(result.status, WorkerStatus::Completed);
    assert_eq!(result.commits, vec!["commit-aaa".to_string()]);

    let record = ledger.project().expect("projection");
    assert_eq!(record.task_id, "task-400");
    assert_eq!(record.prior_worker.as_deref(), Some("mock-worker"));
    assert_eq!(
        record.external_session_id.as_deref(),
        Some("mock-session-123")
    );
    assert_eq!(record.commits, vec!["commit-aaa".to_string()]);
    assert_eq!(record.worktree, temp.path().to_string_lossy());

    // Verify request delivered to worker
    let reqs = mock.received_requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].task_id, "task-400");
    assert_eq!(reqs[0].worktree, temp.path().to_string_lossy());
    assert!(
        temp.path()
            .join(".liberado/tasks/task-400/ledger.jsonl")
            .is_file()
    );
}

#[test]
fn supervisor_handle_ci_failure_triggers_worker_kickback() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mock = std::sync::Arc::new(MockWorker::new(vec![
        vec!["commit-init".into()],
        vec!["commit-repair".into()],
    ]));
    let supervisor = ControlPlaneSupervisor::new(mock.clone());

    let req = DispatchTaskRequest::new(
        "task-401",
        "Add telemetry tracing",
        temp.path().to_string_lossy(),
        "feat/tracing",
        "main",
    )
    .with_acceptance_criteria(vec!["Trace spans must be named".into()]);

    let (mut ledger, _result) = supervisor.dispatch_task(&req).expect("dispatch");

    // Handle CI failure kickback
    let repair_result = supervisor
        .handle_ci_failure(
            &mut ledger,
            vec!["test_trace_span_timing".into()],
            Some("assertion failed: duration > 0".into()),
        )
        .expect("handle CI failure");

    assert_eq!(repair_result.status, WorkerStatus::Completed);

    // Verify worker resume call
    let resumes = mock.received_resumes.lock().unwrap();
    assert_eq!(resumes.len(), 1);
    let (handle, event) = &resumes[0];
    assert_eq!(handle.task_id, "task-401");
    assert_eq!(
        handle.external_session_id.as_deref(),
        Some("mock-session-123")
    );
    assert_eq!(handle.worktree, temp.path().to_string_lossy());
    assert!(
        handle
            .continuation_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("Add telemetry tracing"))
    );

    assert!(matches!(
        event.payload,
        TaskEventKind::CiFailed { ref failures, .. } if failures == &["test_trace_span_timing"]
    ));

    // Project ledger and verify state
    let record = ledger.project().expect("projection");
    assert_eq!(record.commits.len(), 2);
    assert_eq!(
        record.external_session_id.as_deref(),
        Some("mock-session-123")
    );
}

#[test]
fn supervisor_restarts_without_session_using_full_task_context() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mock = std::sync::Arc::new(MockWorker::without_session(vec![Vec::new(), Vec::new()]));
    let supervisor = ControlPlaneSupervisor::new(mock.clone());
    let req = DispatchTaskRequest::new(
        "task-fresh-repair",
        "Keep the objective across workers",
        temp.path().to_string_lossy(),
        "feat/context",
        "main",
    )
    .with_acceptance_criteria(vec!["The repair sees this criterion".into()]);
    let (mut ledger, _) = supervisor.dispatch_task(&req).expect("dispatch");

    supervisor
        .handle_ci_failure(
            &mut ledger,
            vec!["context_test".into()],
            Some("expected context".into()),
        )
        .expect("fresh repair");

    assert!(mock.received_resumes.lock().unwrap().is_empty());
    let requests = mock.received_requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let prompt = &requests[1].prompt;
    assert!(prompt.contains("Keep the objective across workers"));
    assert!(prompt.contains("The repair sees this criterion"));
    assert!(prompt.contains("context_test"));
    assert!(prompt.contains("expected context"));
}

#[test]
fn opencode_worker_config_defaults() {
    let cfg = OpenCodeWorkerConfig::default();
    assert_eq!(cfg.model, "openrouter/~deepseek/deepseek-v4-flash-latest");
    assert!(cfg.auto_approve);
    assert!(cfg.executable.is_none());
}

#[test]
fn coder_tuning_builds_a_named_control_plane_worker_registry() {
    let value: toml::Value = toml::from_str(
        r#"
[control_plane]
default_worker = "paid-opencode"

[control_plane.workers.paid-opencode]
kind = "open_code"
executable = "opencode-custom"
model = "openrouter/test-model"
auto_approve = false
"#,
    )
    .expect("control-plane TOML");

    let tuning = CoderTuning::from_value(Some(&value)).expect("valid worker config");
    assert_eq!(tuning.control_plane.default_worker, "paid-opencode");
    assert!(matches!(
        tuning.control_plane.workers.get("paid-opencode"),
        Some(WorkerAdapterConfig::OpenCode {
            executable: Some(executable),
            model,
            auto_approve: false,
        }) if executable == "opencode-custom" && model == "openrouter/test-model"
    ));
}

#[test]
fn control_plane_rejects_an_unknown_default_worker_at_load_time() {
    let value: toml::Value = toml::from_str(
        r#"
[control_plane]
default_worker = "missing"
"#,
    )
    .expect("control-plane TOML");

    let error = CoderTuning::from_value(Some(&value)).expect_err("unknown worker must fail");
    assert!(
        error.to_string().contains("names no configured worker"),
        "{error}"
    );
}

#[test]
fn opencode_start_returns_running_handle_and_cancel_terminates_it() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let path = temp.path();
    for args in [
        vec!["init"],
        vec!["config", "user.name", "Test Agent"],
        vec!["config", "user.email", "agent@test.local"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .expect("git setup")
                .success()
        );
    }
    std::fs::write(path.join("README.md"), "test").expect("write fixture");
    for args in [vec!["add", "README.md"], vec!["commit", "-m", "initial"]] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .expect("git commit")
                .success()
        );
    }

    #[cfg(windows)]
    let executable = {
        let script = path.join("fake-opencode.cmd");
        std::fs::write(&script, "@echo off\r\nping -n 30 127.0.0.1 >nul\r\n")
            .expect("write fake ACP server");
        script
    };
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        let script = path.join("fake-opencode");
        std::fs::write(&script, "#!/bin/sh\nsleep 30 >/dev/null 2>&1\n")
            .expect("write fake ACP server");
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        script
    };

    let worker = OpenCodeWorker::new(OpenCodeWorkerConfig {
        executable: Some(executable.to_string_lossy().into_owned()),
        ..OpenCodeWorkerConfig::default()
    });
    let request = WorkerRunRequest {
        task_id: "task-cancel".into(),
        objective: "cancel test".into(),
        worktree: path.to_string_lossy().into_owned(),
        branch: "master".into(),
        base_ref: "HEAD".into(),
        prompt: "wait".into(),
        session_id: None,
    };

    let started = std::time::Instant::now();
    let handle = worker.start(&request).expect("start worker");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(worker.status(&handle).unwrap(), WorkerStatus::Running);
    worker.cancel(&handle).expect("cancel worker");
    let result = worker.collect(&handle).expect("collect cancellation");
    assert_eq!(result.status, WorkerStatus::Failed);
    assert_eq!(
        result.blocking_issue.as_deref(),
        Some("worker run was cancelled")
    );

    let prior_handle = RunHandle::new(
        "prior-run",
        worker.id(),
        "task-cancel",
        path.to_string_lossy(),
    )
    .with_session_id("session-to-resume")
    .with_continuation_prompt("Original task context");
    let failure = TaskEvent::new(
        "evt-resume-test",
        "task-cancel",
        TaskEventKind::CiFailed {
            run_id: Some("prior-run".into()),
            failures: vec!["resume_test".into()],
            failure_log_excerpt: Some("repair this failure".into()),
        },
    );
    let resumed = worker
        .resume(&prior_handle, &failure)
        .expect("resume worker");
    assert_eq!(
        resumed.external_session_id.as_deref(),
        Some("session-to-resume")
    );
    assert!(
        resumed
            .continuation_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("Original task context"))
    );
    worker.cancel(&resumed).expect("cancel resumed worker");
    assert_eq!(
        worker
            .collect(&resumed)
            .expect("collect resumed worker")
            .status,
        WorkerStatus::Failed
    );
}

#[test]
fn opencode_read_response_until_id_matches_target() {
    let input = "{\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{\"status\":\"ok\"}}\n";
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
    let val = opencode::read_response_until_id(&mut reader, 99).expect("read response");
    assert_eq!(val["status"], "ok");
}

#[test]
fn opencode_read_response_until_id_returns_error_on_rpc_error() {
    let input = "{\"jsonrpc\":\"2.0\",\"id\":99,\"error\":{\"message\":\"failed\"}}\n";
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
    let err = opencode::read_response_until_id(&mut reader, 99).expect_err("should return error");
    assert!(err.to_string().contains("RPC error response"));
}

#[test]
fn opencode_read_response_until_id_handles_eof() {
    let input = "{\"jsonrpc\":\"2.0\",\"id\":1}\n";
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
    let err =
        opencode::read_response_until_id(&mut reader, 2).expect_err("should return EOF error");
    assert!(matches!(err, ControlPlaneError::Io(_)));
}

#[test]
fn opencode_init_acp_session_new_and_resumed() {
    // 1. New session
    let responses = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"ses_new_123\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n"
    );
    let mut stdin = Vec::new();
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(responses));
    let session_id = opencode::init_acp_session(
        &mut stdin,
        &mut reader,
        "worktrees/wt",
        "openrouter/~deepseek/deepseek-v4-flash-latest",
        None,
    )
    .expect("init new session");
    assert_eq!(session_id, "ses_new_123");

    // 2. Resumed session
    let resumed_responses = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n"
    );
    let mut stdin_resumed = Vec::new();
    let mut reader_resumed = std::io::BufReader::new(std::io::Cursor::new(resumed_responses));
    let resumed_id = opencode::init_acp_session(
        &mut stdin_resumed,
        &mut reader_resumed,
        "worktrees/wt",
        "openrouter/~deepseek/deepseek-v4-flash-latest",
        Some("ses_resumed_999"),
    )
    .expect("init resumed session");
    assert_eq!(resumed_id, "ses_resumed_999");
    let resumed_requests = String::from_utf8(stdin_resumed).expect("utf8 requests");
    assert!(resumed_requests.contains("\"method\":\"session/load\""));
    assert!(resumed_requests.contains("\"sessionId\":\"ses_resumed_999\""));
}

#[test]
fn opencode_init_starts_fresh_when_session_load_is_rejected() {
    let responses = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"message\":\"unknown session\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{\"sessionId\":\"ses_fresh\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n"
    );
    let mut stdin = Vec::new();
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(responses));
    let session_id = opencode::init_acp_session(
        &mut stdin,
        &mut reader,
        "worktrees/wt",
        "model",
        Some("ses_missing"),
    )
    .expect("fall back to a new session");
    assert_eq!(session_id, "ses_fresh");
    let requests = String::from_utf8(stdin).expect("utf8 requests");
    assert!(requests.contains("\"method\":\"session/load\""));
    assert!(requests.contains("\"method\":\"session/new\""));
}

#[test]
fn opencode_drain_prompt_turn_handles_permissions_and_chunks() {
    let input = concat!(
        "not valid json\n",
        "{\"jsonrpc\":\"2.0\",\"id\":50,\"method\":\"session/request_permission\",\"params\":{\"options\":[{\"optionId\":\"allow-once\",\"kind\":\"allow_once\"}]}}\n",
        "{\"params\":{\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"text\":\"hello \"}}}}\n",
        "{\"params\":{\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"text\":\"world\"}}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"stopReason\":\"end_turn\"}}\n"
    );
    let mut stdin = Vec::new();
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
    let (summary, stop_reason) =
        opencode::drain_prompt_turn(&mut stdin, &mut reader, true).expect("drain prompt");
    assert_eq!(summary, "hello world");
    assert_eq!(stop_reason, "end_turn");
    let permission_response = String::from_utf8(stdin).expect("utf8 response");
    assert!(permission_response.contains("\"outcome\":\"selected\""));
    assert!(permission_response.contains("\"optionId\":\"allow-once\""));
}

#[test]
fn opencode_permission_rejection_uses_cancelled_outcome() {
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":50,\"method\":\"session/request_permission\",\"params\":{\"options\":[]}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"stopReason\":\"end_turn\"}}\n"
    );
    let mut stdin = Vec::new();
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
    opencode::drain_prompt_turn(&mut stdin, &mut reader, false).expect("drain prompt");
    let response = String::from_utf8(stdin).expect("utf8 response");
    assert!(response.contains("\"outcome\":\"cancelled\""));
}

#[test]
fn opencode_prompt_eof_and_rpc_error_are_failures() {
    let mut empty = std::io::BufReader::new(std::io::Cursor::new(""));
    let eof = opencode::drain_prompt_turn(&mut Vec::new(), &mut empty, true)
        .expect_err("EOF before the response must fail");
    assert!(eof.to_string().contains("closed stdout"));

    let input = "{\"jsonrpc\":\"2.0\",\"id\":4,\"error\":{\"message\":\"boom\"}}\n";
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
    let rpc = opencode::drain_prompt_turn(&mut Vec::new(), &mut reader, true)
        .expect_err("RPC error must fail");
    assert!(rpc.to_string().contains("session/prompt failed"));
}

#[test]
fn opencode_synthesize_resume_prompt_variants() {
    let ci_event = TaskEvent::new(
        "evt-ci",
        "task-1",
        TaskEventKind::CiFailed {
            run_id: None,
            failures: vec!["test_fail".into()],
            failure_log_excerpt: Some("panic at test_fail".into()),
        },
    );
    let p1 = opencode::synthesize_resume_prompt(&ci_event);
    assert!(p1.contains("test_fail"));
    assert!(p1.contains("panic at test_fail"));

    let review_event = TaskEvent::new(
        "evt-rev",
        "task-1",
        TaskEventKind::ReviewRejected {
            reviewer: "gatekeeper".into(),
            round: 1,
            diagnosis: "missing docs".into(),
        },
    );
    let p2 = opencode::synthesize_resume_prompt(&review_event);
    assert!(p2.contains("missing docs"));

    let other_event = TaskEvent::new("evt-other", "task-1", TaskEventKind::CiPassed);
    let p3 = opencode::synthesize_resume_prompt(&other_event);
    assert_eq!(p3, "Continue working on the task.");
}

#[test]
fn opencode_build_worker_result_variants() {
    let res_ok = opencode::build_worker_result(
        "end_turn",
        "all done",
        vec!["commit-1".into()],
        vec!["src/lib.rs".into()],
        "ses-1".into(),
    );
    assert_eq!(res_ok.status, WorkerStatus::Completed);
    assert!(res_ok.blocking_issue.is_none());

    let res_err = opencode::build_worker_result(
        "error: timeout",
        "failed",
        Vec::new(),
        Vec::new(),
        "ses-1".into(),
    );
    assert_eq!(res_err.status, WorkerStatus::Failed);
    assert_eq!(
        res_err.blocking_issue.as_deref(),
        Some("worker stopped with reason 'error: timeout'")
    );
}

#[test]
fn opencode_inspect_git_worktree_runs_safely() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let path = temp.path();

    let _ = std::process::Command::new("git")
        .args(["init"])
        .current_dir(path)
        .status();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "Test Agent"])
        .current_dir(path)
        .status();
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "agent@test.local"])
        .current_dir(path)
        .status();

    std::fs::write(path.join("file.txt"), "hello").unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(path)
        .status();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(path)
        .status();

    let baseline = opencode::capture_git_snapshot(path.to_str().unwrap()).expect("snapshot");
    let (commits, files) =
        opencode::inspect_git_worktree(path.to_str().unwrap(), &baseline).expect("inspect no-op");
    assert!(commits.is_empty(), "the pre-run HEAD is not worker output");
    assert!(files.is_empty());

    std::fs::write(path.join("worker.txt"), "worker output").unwrap();
    let (commits, files) =
        opencode::inspect_git_worktree(path.to_str().unwrap(), &baseline).expect("inspect dirty");
    assert!(commits.is_empty());
    assert_eq!(files, vec!["worker.txt"]);

    std::process::Command::new("git")
        .args(["add", "worker.txt"])
        .current_dir(path)
        .status()
        .expect("git add worker output");
    std::process::Command::new("git")
        .args(["commit", "-m", "worker output"])
        .current_dir(path)
        .status()
        .expect("git commit worker output");
    let (commits, files) =
        opencode::inspect_git_worktree(path.to_str().unwrap(), &baseline).expect("inspect commit");
    assert_eq!(commits.len(), 1);
    assert_eq!(files, vec!["worker.txt"]);
}

#[test]
fn opencode_run_acp_session_round_trip() {
    let responses = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"ses_run_456\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n",
        "{\"params\":{\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"text\":\"all set\"}}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"stopReason\":\"end_turn\"}}\n"
    );
    let mut stdin = Vec::new();
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(responses));
    let (session_id, summary, stop_reason) = opencode::run_acp_session(
        &mut stdin,
        &mut reader,
        "worktrees/wt",
        "openrouter/~deepseek/deepseek-v4-flash-latest",
        None,
        "write test",
        true,
    )
    .expect("run session");

    assert_eq!(session_id, "ses_run_456");
    assert_eq!(summary, "all set");
    assert_eq!(stop_reason, "end_turn");
}

#[test]
fn supervisor_start_run_uses_session_prompt_and_records_lifecycle() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mock = std::sync::Arc::new(MockWorker::new(vec![vec!["commit-bbb".into()]]));
    let supervisor = ControlPlaneSupervisor::new(mock.clone());
    let req = DispatchTaskRequest::new(
        "task-session",
        "Keep the session prompt",
        temp.path().to_string_lossy(),
        "feat/session-prompt",
        "main",
    )
    .with_acceptance_criteria(vec!["Session markdown is preserved".into()]);

    let mut run = supervisor
        .start_run(&req, Some("# Task\nKeep the session prompt".into()))
        .expect("start supervised run");
    assert!(
        run.ledger()
            .events()
            .iter()
            .any(|event| matches!(event.payload, TaskEventKind::WorkerStarted { .. }))
    );
    let result = supervisor.finish_run(&mut run).expect("finish");
    assert_eq!(result.commits, vec!["commit-bbb".to_string()]);
    let reqs = mock.received_requests.lock().unwrap();
    assert_eq!(reqs[0].prompt, "# Task\nKeep the session prompt");
    assert!(
        temp.path()
            .join(".liberado/tasks/task-session/task.json")
            .is_file()
    );
}
