//! Control plane supervisor orchestrating tasks, ledgers, and worker kickbacks.
//!
//! Owns dispatching tasks to worker ports, recording events to the durable ledger,
//! retaining external session identifiers, and driving automated kickbacks when CI fails.

use super::{
    ContinuationContextBuilder, ControlPlaneError, DispatchTaskRequest, RunHandle, TaskEvent,
    TaskEventKind, TaskLedger, WorkerPort, WorkerRunRequest, WorkerRunResult, WorkerStatus,
    tasks_root_from_worktree,
};
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;

/// Orchestrates task lifecycles, event ledgers, and worker dispatch.
pub struct ControlPlaneSupervisor {
    worker: Arc<dyn WorkerPort>,
}

/// One in-flight worker run plus the durable ledger that records its lifecycle.
pub struct SupervisedRun {
    handle: RunHandle,
    ledger: TaskLedger,
}

impl SupervisedRun {
    /// Handle the worker returned for this run.
    pub fn handle(&self) -> &RunHandle {
        &self.handle
    }

    /// Durable ledger for this task.
    pub fn ledger(&self) -> &TaskLedger {
        &self.ledger
    }

    /// Consume the run and return the ledger after the worker has finished.
    pub fn into_ledger(self) -> TaskLedger {
        self.ledger
    }
}

impl ControlPlaneSupervisor {
    pub fn new(worker: Arc<dyn WorkerPort>) -> Self {
        Self { worker }
    }

    /// Dispatch a new task to the configured worker port and initialize its event ledger.
    pub fn dispatch_task(
        &self,
        req: &DispatchTaskRequest,
    ) -> Result<(TaskLedger, WorkerRunResult), ControlPlaneError> {
        let mut run = self.start_run(req, None)?;
        let result = self.finish_run(&mut run)?;
        Ok((run.into_ledger(), result))
    }

    /// Create the durable ledger and start the worker without blocking on collect.
    ///
    /// `prompt` overrides the continuation builder so a session pack can keep its
    /// existing task markdown (context, instructions, prior feedback).
    pub fn start_run(
        &self,
        req: &DispatchTaskRequest,
        prompt: Option<String>,
    ) -> Result<SupervisedRun, ControlPlaneError> {
        let mut ledger = create_task_ledger(req)?;
        let prompt = match prompt {
            Some(prompt) => prompt,
            None => ContinuationContextBuilder::build(&ledger.project()?),
        };
        let handle = self.worker.start(&worker_request(req, prompt))?;
        record_worker_started(&mut ledger, &handle, self.worker.id())?;
        Ok(SupervisedRun { handle, ledger })
    }

    /// Poll the worker for the current run status.
    pub fn poll_status(&self, run: &SupervisedRun) -> Result<WorkerStatus, ControlPlaneError> {
        self.worker.status(&run.handle)
    }

    /// Ask the worker to stop the run. The caller still collects to record the finish event.
    pub fn cancel_run(&self, run: &SupervisedRun) -> Result<(), ControlPlaneError> {
        self.worker.cancel(&run.handle)
    }

    /// Collect the worker result and append finish plus commit events to the ledger.
    pub fn finish_run(
        &self,
        run: &mut SupervisedRun,
    ) -> Result<WorkerRunResult, ControlPlaneError> {
        let result = self.worker.collect(&run.handle)?;
        record_finished_run(&mut run.ledger, &run.handle, &result)?;
        Ok(result)
    }

    /// Automatically handle a CI failure on an existing task ledger by kicking back to the worker.
    pub fn handle_ci_failure(
        &self,
        ledger: &mut TaskLedger,
        failures: Vec<String>,
        failure_log_excerpt: Option<String>,
    ) -> Result<WorkerRunResult, ControlPlaneError> {
        let record = ledger.project()?;

        let fail_evt = TaskEvent::new(
            format!("evt-cifail-{}", Utc::now().timestamp_millis()),
            &record.task_id,
            TaskEventKind::CiFailed {
                run_id: None,
                failures: failures.clone(),
                failure_log_excerpt,
            },
        );

        ledger.append(fail_evt.clone())?;
        let updated_record = ledger.project()?;

        let continuation_prompt = ContinuationContextBuilder::build(&updated_record);
        let can_resume = updated_record.prior_worker.as_deref() == Some(self.worker.id())
            && updated_record
                .external_session_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty());
        let resume_handle = if can_resume {
            let session_id = updated_record.external_session_id.clone().ok_or_else(|| {
                ControlPlaneError::Protocol("resumable task omitted its session id".into())
            })?;
            let handle = RunHandle::new(
                format!("run-kickback-{}", Utc::now().timestamp_millis()),
                self.worker.id(),
                &updated_record.task_id,
                updated_record.worktree.clone(),
            )
            .with_session_id(session_id)
            .with_continuation_prompt(continuation_prompt);
            self.worker.resume(&handle, &fail_evt)?
        } else {
            self.worker.start(&WorkerRunRequest {
                task_id: updated_record.task_id.clone(),
                objective: updated_record.objective.clone(),
                worktree: updated_record.worktree.clone(),
                branch: updated_record.branch.clone(),
                base_ref: updated_record.base_ref.clone(),
                prompt: continuation_prompt,
                session_id: None,
            })?
        };

        ledger.append(TaskEvent::new(
            format!("evt-{}-resumed", resume_handle.run_id),
            &updated_record.task_id,
            TaskEventKind::WorkerResumed {
                run_id: resume_handle.run_id.clone(),
                worker_id: self.worker.id().to_string(),
                reason: format!("CI failure repair: {} failures", failures.len()),
            },
        ))?;

        let result = self.worker.collect(&resume_handle)?;
        append_worker_finished(ledger, &resume_handle, &result)?;
        append_commits(ledger, &resume_handle, &result, "repair-commit")?;
        Ok(result)
    }
}

fn create_task_ledger(req: &DispatchTaskRequest) -> Result<TaskLedger, ControlPlaneError> {
    let initial_event = TaskEvent::new(
        format!("evt-{}-created", req.task_id),
        &req.task_id,
        TaskEventKind::TaskCreated {
            objective: req.objective.clone(),
            acceptance_criteria: req.acceptance_criteria.clone(),
            worktree: req.worktree.clone(),
            branch: req.branch.clone(),
            base_ref: req.base_ref.clone(),
            repo: req.repo.clone(),
        },
    );
    let tasks_root = req
        .ledger_root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| tasks_root_from_worktree(&req.worktree));
    TaskLedger::create_in(tasks_root, initial_event)
}

fn worker_request(req: &DispatchTaskRequest, prompt: String) -> WorkerRunRequest {
    WorkerRunRequest {
        task_id: req.task_id.clone(),
        objective: req.objective.clone(),
        worktree: req.worktree.clone(),
        branch: req.branch.clone(),
        base_ref: req.base_ref.clone(),
        prompt,
        session_id: None,
    }
}

fn record_worker_started(
    ledger: &mut TaskLedger,
    handle: &RunHandle,
    worker_id: &str,
) -> Result<(), ControlPlaneError> {
    ledger.append(TaskEvent::new(
        format!("evt-{}-started", handle.run_id),
        &handle.task_id,
        TaskEventKind::WorkerStarted {
            run_id: handle.run_id.clone(),
            worker_id: worker_id.to_string(),
            resumed_session_id: handle.external_session_id.clone(),
        },
    ))
}

fn record_finished_run(
    ledger: &mut TaskLedger,
    handle: &RunHandle,
    result: &WorkerRunResult,
) -> Result<(), ControlPlaneError> {
    append_worker_finished(ledger, handle, result)?;
    append_commits(ledger, handle, result, "commit")
}

fn append_worker_finished(
    ledger: &mut TaskLedger,
    handle: &RunHandle,
    result: &WorkerRunResult,
) -> Result<(), ControlPlaneError> {
    ledger.append(TaskEvent::new(
        format!("evt-{}-finished", handle.run_id),
        &handle.task_id,
        TaskEventKind::WorkerFinished {
            run_id: handle.run_id.clone(),
            status: result.status,
            external_session_id: result.external_session_id.clone(),
            blocking_issue: result.blocking_issue.clone(),
        },
    ))
}

fn append_commits(
    ledger: &mut TaskLedger,
    handle: &RunHandle,
    result: &WorkerRunResult,
    kind: &str,
) -> Result<(), ControlPlaneError> {
    for commit in &result.commits {
        ledger.append(TaskEvent::new(
            format!("evt-{}-{kind}", &commit[..commit.len().min(8)]),
            &handle.task_id,
            TaskEventKind::CommitProduced {
                commit_sha: commit.clone(),
                message: result.summary.clone(),
                files_changed: result.files_changed.clone(),
            },
        ))?;
    }
    Ok(())
}
