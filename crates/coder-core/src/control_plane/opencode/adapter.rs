use super::{
    build_worker_result, capture_git_snapshot, failed_worker_result, inspect_git_worktree,
    next_run_id, process_tree::ContainedProcess, run_acp_session, synthesize_resume_prompt,
    take_stdio,
};
use crate::control_plane::{
    ControlPlaneError, RunHandle, TaskEvent, WorkerPort, WorkerRunRequest, WorkerRunResult,
    WorkerStatus,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};

#[derive(Debug)]
struct RunState {
    status: WorkerStatus,
    result: Option<WorkerRunResult>,
}

#[derive(Debug)]
struct ActiveRun {
    state: Mutex<RunState>,
    ready: Condvar,
    child: Mutex<Option<ContainedProcess>>,
    cancelled: AtomicBool,
}

impl ActiveRun {
    fn new(child: ContainedProcess) -> Self {
        Self {
            state: Mutex::new(RunState {
                status: WorkerStatus::Running,
                result: None,
            }),
            ready: Condvar::new(),
            child: Mutex::new(Some(child)),
            cancelled: AtomicBool::new(false),
        }
    }

    fn finish(&self, result: WorkerRunResult) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.status = result.status;
        state.result = Some(result);
        self.ready.notify_all();
    }

    fn terminate(&self) {
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(mut child) = child.take() {
            child.terminate();
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct GitSnapshot {
    pub(super) head: String,
}

/// Configuration for the OpenCode external worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenCodeWorkerConfig {
    pub executable: Option<String>,
    pub model: String,
    pub auto_approve: bool,
}

impl Default for OpenCodeWorkerConfig {
    fn default() -> Self {
        Self {
            executable: None,
            model: "openrouter/~deepseek/deepseek-v4-flash-latest".into(),
            auto_approve: true,
        }
    }
}

/// Worker implementation driving OpenCode via ACP JSON-RPC.
pub struct OpenCodeWorker {
    config: OpenCodeWorkerConfig,
    runs: Arc<RwLock<HashMap<String, Arc<ActiveRun>>>>,
}

impl OpenCodeWorker {
    pub fn new(config: OpenCodeWorkerConfig) -> Self {
        Self {
            config,
            runs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn config(&self) -> &OpenCodeWorkerConfig {
        &self.config
    }

    /// Spawns the ACP server process in its own process group or job object.
    fn spawn_acp_process(&self, worktree: &str) -> Result<ContainedProcess, ControlPlaneError> {
        let default_executable = if cfg!(windows) {
            "opencode.cmd"
        } else {
            "opencode"
        };
        let executable = self
            .config
            .executable
            .as_deref()
            .unwrap_or(default_executable);
        ContainedProcess::spawn_acp(executable, worktree)
    }

    fn launch_turn(
        &self,
        task_id: &str,
        worktree: &str,
        prompt: &str,
        resumed_session_id: Option<&str>,
    ) -> Result<RunHandle, ControlPlaneError> {
        let baseline = capture_git_snapshot(worktree)?;
        let mut child = self.spawn_acp_process(worktree)?;
        let (mut stdin, mut reader) = match take_stdio(child.child_mut()) {
            Ok(streams) => streams,
            Err(error) => {
                child.terminate();
                return Err(error);
            }
        };

        let run_id = next_run_id();
        let active = Arc::new(ActiveRun::new(child));
        self.runs
            .write()
            .map_err(|_| ControlPlaneError::Protocol("worker run registry is poisoned".into()))?
            .insert(run_id.clone(), active.clone());

        let worktree_owned = worktree.to_string();
        let model = self.config.model.clone();
        let prompt_owned = prompt.to_string();
        let resumed_session_id = resumed_session_id.map(str::to_string);
        let session_id_for_turn = resumed_session_id.clone();
        let auto_approve = self.config.auto_approve;
        std::thread::spawn(move || {
            let outcome = run_acp_session(
                &mut stdin,
                &mut reader,
                &worktree_owned,
                &model,
                session_id_for_turn.as_deref(),
                &prompt_owned,
                auto_approve,
            );
            drop(stdin);
            active.terminate();

            let result = if active.cancelled.load(Ordering::SeqCst) {
                failed_worker_result("worker run was cancelled", None)
            } else {
                match outcome {
                    Ok((session_id, assistant_summary, stop_reason)) => {
                        match inspect_git_worktree(&worktree_owned, &baseline) {
                            Ok((commits, files_changed)) => build_worker_result(
                                &stop_reason,
                                &assistant_summary,
                                commits,
                                files_changed,
                                session_id,
                            ),
                            Err(error) => {
                                failed_worker_result(&error.to_string(), Some(session_id))
                            }
                        }
                    }
                    Err(error) => failed_worker_result(&error.to_string(), None),
                }
            };
            active.finish(result);
        });

        Ok(RunHandle {
            run_id,
            worker_id: self.id().to_string(),
            task_id: task_id.to_string(),
            external_session_id: resumed_session_id,
            worktree: worktree.to_string(),
            continuation_prompt: Some(prompt.to_string()),
        })
    }
}

impl WorkerPort for OpenCodeWorker {
    fn id(&self) -> &str {
        "opencode"
    }

    fn start(&self, req: &WorkerRunRequest) -> Result<RunHandle, ControlPlaneError> {
        self.launch_turn(
            &req.task_id,
            &req.worktree,
            &req.prompt,
            req.session_id.as_deref(),
        )
    }

    fn resume(
        &self,
        handle: &RunHandle,
        event: &TaskEvent,
    ) -> Result<RunHandle, ControlPlaneError> {
        let fallback_prompt = synthesize_resume_prompt(event);
        let resume_prompt = match &handle.continuation_prompt {
            Some(context) => format!("{context}\n\n## New Task Event\n{fallback_prompt}"),
            None => fallback_prompt,
        };
        self.launch_turn(
            &handle.task_id,
            &handle.worktree,
            &resume_prompt,
            handle.external_session_id.as_deref(),
        )
    }

    fn status(&self, handle: &RunHandle) -> Result<WorkerStatus, ControlPlaneError> {
        let run = self
            .runs
            .read()
            .map_err(|_| ControlPlaneError::Protocol("worker run registry is poisoned".into()))?
            .get(&handle.run_id)
            .cloned()
            .ok_or_else(|| ControlPlaneError::RunNotFound(handle.run_id.clone()))?;
        let state = run
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        Ok(state.status)
    }

    fn cancel(&self, handle: &RunHandle) -> Result<(), ControlPlaneError> {
        let run = self
            .runs
            .read()
            .map_err(|_| ControlPlaneError::Protocol("worker run registry is poisoned".into()))?
            .get(&handle.run_id)
            .cloned()
            .ok_or_else(|| ControlPlaneError::RunNotFound(handle.run_id.clone()))?;
        run.cancelled.store(true, Ordering::SeqCst);
        run.terminate();
        Ok(())
    }

    fn collect(&self, handle: &RunHandle) -> Result<WorkerRunResult, ControlPlaneError> {
        let run = self
            .runs
            .read()
            .map_err(|_| ControlPlaneError::Protocol("worker run registry is poisoned".into()))?
            .get(&handle.run_id)
            .cloned()
            .ok_or_else(|| ControlPlaneError::RunNotFound(handle.run_id.clone()))?;
        let mut state = run
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while state.result.is_none() {
            state = run
                .ready
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        let result = state
            .result
            .take()
            .ok_or_else(|| ControlPlaneError::RunNotFound(handle.run_id.clone()))?;
        drop(state);
        self.runs
            .write()
            .map_err(|_| ControlPlaneError::Protocol("worker run registry is poisoned".into()))?
            .remove(&handle.run_id);
        Ok(result)
    }
}
