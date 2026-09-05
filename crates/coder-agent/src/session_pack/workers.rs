//! Configured coding-worker selection for the session pack.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use liberado_coder_core::{
    CoderBackend, CoderError, CoderRunRequest, CoderRunResult, ControlPlaneConfig,
    ControlPlaneSupervisor, DispatchTaskRequest, NATIVE_WORKER_ID, SupervisedRun, WorkerPort,
    WorkerRunResult, WorkerStatus,
};
use liberado_common::Outcome;

/// Native plus configured external workers, resolved once when tuning is applied.
pub(super) struct WorkerRegistry {
    default_worker: String,
    external: BTreeMap<String, Arc<dyn WorkerPort>>,
}

impl Default for WorkerRegistry {
    fn default() -> Self {
        Self {
            default_worker: NATIVE_WORKER_ID.into(),
            external: BTreeMap::new(),
        }
    }
}

impl WorkerRegistry {
    pub(super) fn from_config(config: &ControlPlaneConfig) -> Self {
        let external = config
            .workers
            .iter()
            .map(|(name, worker)| (name.clone(), worker.build()))
            .collect();
        Self {
            default_worker: config.default_worker.clone(),
            external,
        }
    }

    pub(super) fn register(&mut self, name: impl Into<String>, worker: Arc<dyn WorkerPort>) {
        self.external.insert(name.into(), worker);
    }

    /// Payload wins over the session profile, then the configured default applies.
    pub(super) fn select(
        &self,
        overrides: &serde_json::Value,
        payload: &serde_json::Value,
    ) -> Result<String, CoderError> {
        let selected = worker_name(payload)
            .or_else(|| worker_name(overrides))
            .unwrap_or(&self.default_worker);
        if selected == NATIVE_WORKER_ID || self.external.contains_key(selected) {
            Ok(selected.to_string())
        } else {
            Err(CoderError::Backend(format!(
                "coding worker '{selected}' is not configured"
            )))
        }
    }

    pub(super) async fn run(
        &self,
        selected: &str,
        native: &Arc<dyn CoderBackend>,
        request: CoderRunRequest,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<RegistryRun, CoderError> {
        if *cancel.borrow() {
            return Ok(RegistryRun::Cancelled);
        }
        if selected == NATIVE_WORKER_ID {
            let run = native.run(request);
            tokio::pin!(run);
            return tokio::select! {
                result = &mut run => result.map(|result| RegistryRun::Finished(Box::new(result))),
                _ = cancellation_requested(cancel) => Ok(RegistryRun::Cancelled),
            };
        }

        let worker = self.external.get(selected).cloned().ok_or_else(|| {
            CoderError::Backend(format!("coding worker '{selected}' is not configured"))
        })?;
        run_external(worker, request, cancel).await
    }
}

pub(super) enum RegistryRun {
    Finished(Box<CoderRunResult>),
    Cancelled,
}

fn worker_name(root: &serde_json::Value) -> Option<&str> {
    root.get("control_plane")
        .and_then(|value| value.get("worker"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
}

async fn run_external(
    worker: Arc<dyn WorkerPort>,
    request: CoderRunRequest,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<RegistryRun, CoderError> {
    let supervisor = ControlPlaneSupervisor::new(worker.clone());
    let mut run = start_supervised(&supervisor, &request)?;

    loop {
        let status = supervisor
            .poll_status(&run)
            .map_err(|error| CoderError::Backend(error.to_string()))?;
        if matches!(status, WorkerStatus::Completed | WorkerStatus::Failed) {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            _ = cancellation_requested(cancel) => {
                return cancel_supervised(&supervisor, &mut run);
            }
        }
    }

    let result = supervisor
        .finish_run(&mut run)
        .map_err(|error| CoderError::Backend(error.to_string()))?;
    Ok(RegistryRun::Finished(Box::new(normalize_result(
        worker.id(),
        result,
    ))))
}

fn start_supervised(
    supervisor: &ControlPlaneSupervisor,
    request: &CoderRunRequest,
) -> Result<SupervisedRun, CoderError> {
    supervisor
        .start_run(&dispatch_request(request), Some(external_prompt(request)))
        .map_err(|error| CoderError::Backend(error.to_string()))
}

fn cancel_supervised(
    supervisor: &ControlPlaneSupervisor,
    run: &mut SupervisedRun,
) -> Result<RegistryRun, CoderError> {
    supervisor
        .cancel_run(run)
        .map_err(|error| CoderError::Backend(error.to_string()))?;
    let _ = supervisor.finish_run(run);
    Ok(RegistryRun::Cancelled)
}

async fn cancellation_requested(cancel: &mut tokio::sync::watch::Receiver<bool>) {
    if *cancel.borrow() {
        return;
    }
    while cancel.changed().await.is_ok() {
        if *cancel.borrow() {
            return;
        }
    }
    std::future::pending().await
}

fn dispatch_request(request: &CoderRunRequest) -> DispatchTaskRequest {
    DispatchTaskRequest {
        task_id: request.task.id.clone(),
        objective: request.task.description.clone(),
        acceptance_criteria: request.task.success_criteria.clone(),
        worktree: request.workspace.root.clone(),
        branch: current_branch(&request.workspace.root)
            .unwrap_or_else(|| request.workspace.base_ref.clone()),
        base_ref: request.workspace.base_ref.clone(),
        repo: request.workspace.repo.clone(),
    }
}

fn current_branch(worktree: &str) -> Option<String> {
    let output = liberado_common::process::std_command("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(worktree)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|branch| !branch.is_empty())
}

fn external_prompt(request: &CoderRunRequest) -> String {
    let mut sections = vec![format!("# Task\n{}", request.task.description)];
    if let Some(context) = &request.task.context {
        sections.push(format!("# Context\n{context}"));
    }
    if !request.task.success_criteria.is_empty() {
        sections.push(format!(
            "# Acceptance criteria\n{}",
            request
                .task
                .success_criteria
                .iter()
                .map(|criterion| format!("- {criterion}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if let Some(instructions) = &request.config.coder.prompt {
        sections.push(format!("# Worker instructions\n{instructions}"));
    }
    if !request.prior_feedback.is_empty() {
        sections.push(format!(
            "# Prior feedback\n{}",
            request.prior_feedback.join("\n\n")
        ));
    }
    sections.join("\n\n")
}

fn normalize_result(worker_id: &str, result: WorkerRunResult) -> CoderRunResult {
    let outcome = if result.status == WorkerStatus::Completed {
        Outcome::Succeeded
    } else {
        Outcome::Failed
    };
    CoderRunResult {
        backend: worker_id.to_string(),
        outcome,
        summary: result.summary,
        files_changed: result.files_changed,
        file_changes: Vec::new(),
        validation_notes: result.blocking_issue,
        critic_verdict: None,
        gate_votes: Vec::new(),
        trace_path: None,
        diff_findings: Vec::new(),
        session_findings: Vec::new(),
        remediation: None,
        diagnostics: serde_json::json!({
            "commits": result.commits,
            "tests_run": result.tests_run,
            "tests_passed": result.tests_passed,
            "recommended_next_action": result.recommended_next_action,
            "external_session_id": result.external_session_id,
        }),
    }
}
