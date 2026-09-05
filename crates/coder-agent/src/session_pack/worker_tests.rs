use super::tests::{ScriptedBackend, goal};
use super::*;
use liberado_coder_core::{
    CoderTask, CoderTuning, ControlPlaneConfig, ControlPlaneError, RunHandle, TaskEvent,
    WorkerAdapterConfig, WorkerPort, WorkerRunRequest, WorkerRunResult, WorkerStatus, WorkspaceRef,
};
use liberado_provider::MockProvider;
use liberado_session::HumanInput;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

struct ImmediateWorker {
    starts: AtomicUsize,
    polls: AtomicUsize,
    request: std::sync::Mutex<Option<WorkerRunRequest>>,
}

impl ImmediateWorker {
    fn new() -> Self {
        Self {
            starts: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            request: std::sync::Mutex::new(None),
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
    let workspace_path = std::env::temp_dir();
    let request = crate::assemble_production_run(
        &CoderTuning::default(),
        crate::ProductionSurface {
            task: CoderTask::new("cancel-test", "test cancellation"),
            workspace: WorkspaceRef::new(workspace_path.to_string_lossy(), "HEAD"),
            workspace_path,
            ..crate::ProductionSurface::default()
        },
    )
    .request;
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
