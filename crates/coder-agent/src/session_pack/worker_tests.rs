use super::tests::{ScriptedBackend, goal};
use super::*;
use liberado_coder_core::{
    CoderTask, CoderTuning, ControlPlaneConfig, ControlPlaneError, RunHandle, TaskEvent,
    TaskEventKind, WorkerAdapterConfig, WorkerPort, WorkerRunRequest, WorkerRunResult,
    WorkerStatus, WorkspaceRef,
};
use liberado_provider::MockProvider;
use liberado_session::HumanInput;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

struct ImmediateWorker {
    starts: AtomicUsize,
    polls: AtomicUsize,
    cancels: AtomicUsize,
    request: std::sync::Mutex<Option<WorkerRunRequest>>,
}

impl ImmediateWorker {
    fn new() -> Self {
        Self {
            starts: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            request: std::sync::Mutex::new(None),
        }
    }
}

struct StickyWorker {
    starts: AtomicUsize,
    cancels: AtomicUsize,
    running: std::sync::atomic::AtomicBool,
}

impl StickyWorker {
    fn new() -> Self {
        Self {
            starts: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            running: std::sync::atomic::AtomicBool::new(true),
        }
    }
}

impl WorkerPort for ImmediateWorker {
    fn id(&self) -> &str {
        "test-external"
    }

    fn start(&self, request: &WorkerRunRequest) -> Result<RunHandle, ControlPlaneError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        *self.request.lock().unwrap() = Some(request.clone());
        Ok(RunHandle::new(
            "external-run",
            self.id(),
            &request.task_id,
            &request.worktree,
        ))
    }

    fn resume(
        &self,
        _handle: &RunHandle,
        _event: &TaskEvent,
    ) -> Result<RunHandle, ControlPlaneError> {
        unreachable!("this scenario starts a new external run")
    }

    fn status(&self, _handle: &RunHandle) -> Result<WorkerStatus, ControlPlaneError> {
        if self.polls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(WorkerStatus::Running)
        } else {
            Ok(WorkerStatus::Completed)
        }
    }

    fn cancel(&self, _handle: &RunHandle) -> Result<(), ControlPlaneError> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn collect(&self, _handle: &RunHandle) -> Result<WorkerRunResult, ControlPlaneError> {
        Ok(WorkerRunResult {
            status: WorkerStatus::Completed,
            summary: "external worker completed".into(),
            commits: vec!["abc123".into()],
            files_changed: vec!["src/lib.rs".into()],
            tests_run: 1,
            tests_passed: 1,
            blocking_issue: None,
            recommended_next_action: None,
            external_session_id: Some("external-session".into()),
        })
    }
}

impl WorkerPort for StickyWorker {
    fn id(&self) -> &str {
        "sticky-external"
    }

    fn start(&self, request: &WorkerRunRequest) -> Result<RunHandle, ControlPlaneError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(RunHandle::new(
            "sticky-run",
            self.id(),
            &request.task_id,
            &request.worktree,
        ))
    }

    fn resume(
        &self,
        _handle: &RunHandle,
        _event: &TaskEvent,
    ) -> Result<RunHandle, ControlPlaneError> {
        unreachable!("this scenario starts a new external run")
    }

    fn status(&self, _handle: &RunHandle) -> Result<WorkerStatus, ControlPlaneError> {
        if self.running.load(Ordering::SeqCst) {
            Ok(WorkerStatus::Running)
        } else {
            Ok(WorkerStatus::Failed)
        }
    }

    fn cancel(&self, _handle: &RunHandle) -> Result<(), ControlPlaneError> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn collect(&self, _handle: &RunHandle) -> Result<WorkerRunResult, ControlPlaneError> {
        Ok(WorkerRunResult {
            status: WorkerStatus::Failed,
            summary: String::new(),
            commits: Vec::new(),
            files_changed: Vec::new(),
            tests_run: 0,
            tests_passed: 0,
            blocking_issue: Some("worker run was cancelled".into()),
            recommended_next_action: None,
            external_session_id: None,
        })
    }
}

fn registry_request(
    task_id: &str,
    workspace: &std::path::Path,
) -> liberado_coder_core::CoderRunRequest {
    crate::assemble_production_run(
        &CoderTuning::default(),
        crate::ProductionSurface {
            task: CoderTask::new(task_id, "test cancellation"),
            workspace: WorkspaceRef::new(workspace.to_string_lossy(), "HEAD"),
            workspace_path: workspace.to_path_buf(),
            ..crate::ProductionSurface::default()
        },
    )
    .request
}

#[test]
fn worker_selection_is_payload_then_profile_then_configured_default() {
    let mut workers = std::collections::BTreeMap::new();
    workers.insert(
        "configured-default".into(),
        WorkerAdapterConfig::OpenCode {
            executable: Some("opencode-test".into()),
            model: "openrouter/test-model".into(),
            auto_approve: false,
        },
    );
    let registry = super::workers::WorkerRegistry::from_config(&ControlPlaneConfig {
        default_worker: "configured-default".into(),
        workers,
    });

    assert_eq!(
        registry
            .select(&serde_json::json!({}), &serde_json::json!({}))
            .unwrap(),
        "configured-default"
    );
    assert_eq!(
        registry
            .select(
                &serde_json::json!({"control_plane": {"worker": "liberado-loop"}}),
                &serde_json::json!({}),
            )
            .unwrap(),
        "liberado-loop"
    );
    assert_eq!(
        registry
            .select(
                &serde_json::json!({"control_plane": {"worker": "liberado-loop"}}),
                &serde_json::json!({"control_plane": {"worker": "configured-default"}}),
            )
            .unwrap(),
        "configured-default"
    );
    assert!(
        registry
            .select(
                &serde_json::json!({"control_plane": {"worker": "missing"}}),
                &serde_json::json!({}),
            )
            .is_err()
    );
}

#[tokio::test]
async fn false_cancel_notification_does_not_stop_an_external_worker() {
    let native: Arc<dyn liberado_coder_core::CoderBackend> = Arc::new(ScriptedBackend {
        seen: Arc::new(std::sync::Mutex::new(Vec::new())),
        fail_attempts: 0,
    });
    let external = Arc::new(ImmediateWorker::new());
    let mut registry = super::workers::WorkerRegistry::default();
    registry.register("configured-worker", external.clone());
    let workspace = tempfile::tempdir().unwrap();
    let request = registry_request("cancel-test", workspace.path());
    let (cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
    cancel_tx.send(false).unwrap();

    let outcome = registry
        .run("configured-worker", &native, request, &mut cancel)
        .await
        .expect("worker result");

    assert!(matches!(outcome, super::workers::RegistryRun::Finished(_)));
    assert_eq!(external.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn profile_override_routes_the_coding_session_to_the_named_worker() {
    let native_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let native = Arc::new(ScriptedBackend {
        seen: native_calls.clone(),
        fail_attempts: 0,
    });
    let external = Arc::new(ImmediateWorker::new());
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::with_backend(native, provider, std::env::temp_dir())
        .with_worker("configured-worker", external.clone());

    let workspace = tempfile::tempdir().unwrap();
    let mut coding_goal = goal("Implement configured worker routing");
    coding_goal.success_criteria = vec!["Use the selected harness".into()];
    coding_goal.payload = serde_json::json!({
        "workspace_root": workspace.path().to_string_lossy(),
        "intake": { "enabled": false },
        "force_host_local": true,
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut stored_goal = coding_goal.clone();
    stored_goal.id = Some("worker-route".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(stored_goal),
    )
    .await;
    let grant = liberado_session::SessionGrant {
        overrides: serde_json::json!({
            "control_plane": { "worker": "configured-worker" }
        }),
        ..Default::default()
    };
    let ctx = PackContext::new(&grant, store, "worker-route");
    let (events, _event_rx) = mpsc::channel::<SessionEvent>(64);
    let (_input_tx, input_rx) = mpsc::channel::<HumanInput>(4);
    let inputs = InputChannel::new(input_rx, None);
    let (cancel_tx, cancel) = tokio::sync::watch::channel(false);

    let result = pack
        .run("worker-route", &coding_goal, &ctx, events, inputs, cancel)
        .await
        .expect("configured worker run");
    drop(cancel_tx);

    assert_eq!(result.terminal, TerminalKind::Succeeded);
    assert_eq!(external.starts.load(Ordering::SeqCst), 1);
    assert!(native_calls.lock().unwrap().is_empty());
    let request = external.request.lock().unwrap().clone().unwrap();
    let branch = liberado_common::process::std_command("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(workspace.path())
        .output()
        .expect("read test branch");
    assert_eq!(
        request.branch,
        String::from_utf8_lossy(&branch.stdout).trim()
    );
    assert!(
        request
            .prompt
            .contains("Implement configured worker routing")
    );
    assert!(request.prompt.contains("Use the selected harness"));
}

#[tokio::test]
async fn external_worker_run_records_durable_ledger_events() {
    let native: Arc<dyn liberado_coder_core::CoderBackend> = Arc::new(ScriptedBackend {
        seen: Arc::new(std::sync::Mutex::new(Vec::new())),
        fail_attempts: 0,
    });
    let external = Arc::new(ImmediateWorker::new());
    let mut registry = super::workers::WorkerRegistry::default();
    registry.register("configured-worker", external.clone());
    let workspace = tempfile::tempdir().unwrap();
    let request = registry_request("ledger-task", workspace.path());
    let (_cancel_tx, mut cancel) = tokio::sync::watch::channel(false);

    let outcome = registry
        .run("configured-worker", &native, request, &mut cancel)
        .await
        .expect("worker result");

    assert!(matches!(outcome, super::workers::RegistryRun::Finished(_)));
    let ledger_path = workspace
        .path()
        .join(".liberado/tasks/ledger-task/ledger.jsonl");
    let projection_path = workspace
        .path()
        .join(".liberado/tasks/ledger-task/task.json");
    assert!(
        ledger_path.is_file(),
        "session path must write ledger.jsonl"
    );
    assert!(
        projection_path.is_file(),
        "session path must write task.json"
    );
    let ledger = liberado_coder_core::TaskLedger::load_from_path(&ledger_path)
        .expect("reload session ledger");
    let kinds: Vec<_> = ledger
        .events()
        .iter()
        .map(|event| std::mem::discriminant(&event.payload))
        .collect();
    assert_eq!(
        kinds[0],
        std::mem::discriminant(&TaskEventKind::TaskCreated {
            objective: String::new(),
            acceptance_criteria: Vec::new(),
            worktree: String::new(),
            branch: String::new(),
            base_ref: String::new(),
            repo: None,
        })
    );
    assert!(
        ledger
            .events()
            .iter()
            .any(|event| matches!(event.payload, TaskEventKind::WorkerStarted { .. }))
    );
    assert!(
        ledger
            .events()
            .iter()
            .any(|event| matches!(event.payload, TaskEventKind::WorkerFinished { .. }))
    );
    let record = ledger.project().expect("projection");
    assert_eq!(record.prior_worker.as_deref(), Some("test-external"));
    assert_eq!(record.commits, vec!["abc123".to_string()]);
}

#[tokio::test]
async fn true_session_cancellation_invokes_worker_port_cancel() {
    let native: Arc<dyn liberado_coder_core::CoderBackend> = Arc::new(ScriptedBackend {
        seen: Arc::new(std::sync::Mutex::new(Vec::new())),
        fail_attempts: 0,
    });
    let external = Arc::new(StickyWorker::new());
    let mut registry = super::workers::WorkerRegistry::default();
    registry.register("configured-worker", external.clone());
    let workspace = tempfile::tempdir().unwrap();
    let request = registry_request("session-cancel", workspace.path());
    let (cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
    let run = registry.run("configured-worker", &native, request, &mut cancel);
    tokio::pin!(run);

    let started = std::time::Instant::now();
    loop {
        tokio::select! {
            outcome = &mut run => panic!(
                "worker finished before cancel: {}",
                outcome
                    .as_ref()
                    .err()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "finished".into())
            ),
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                if external.starts.load(Ordering::SeqCst) > 0 {
                    break;
                }
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(2),
                    "external worker never started"
                );
            }
        }
    }
    cancel_tx.send(true).unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), run)
        .await
        .expect("cancel must finish")
        .expect("worker result");
    assert!(matches!(outcome, super::workers::RegistryRun::Cancelled));
    assert_eq!(external.cancels.load(Ordering::SeqCst), 1);
    let ledger = liberado_coder_core::TaskLedger::load_from_path(
        workspace
            .path()
            .join(".liberado/tasks/session-cancel/ledger.jsonl"),
    )
    .expect("cancelled session still writes a ledger");
    assert!(
        ledger
            .events()
            .iter()
            .any(|event| matches!(event.payload, TaskEventKind::WorkerFinished { .. }))
    );
}
