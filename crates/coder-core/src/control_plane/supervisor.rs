//! Control plane supervisor orchestrating tasks, ledgers, and worker kickbacks.
//!
//! Owns dispatching tasks to worker ports, recording events to the durable ledger,
//! retaining external session identifiers, and driving automated kickbacks when CI fails.

use super::{
    ContinuationContextBuilder, ControlPlaneError, DispatchTaskRequest, RunHandle, TaskEvent,
    TaskEventKind, TaskLedger, WorkerPort, WorkerRunRequest, WorkerRunResult,
};
use chrono::Utc;
use std::path::Path;
use std::sync::Arc;

/// Orchestrates task lifecycles, event ledgers, and worker dispatch.
pub struct ControlPlaneSupervisor {
    worker: Arc<dyn WorkerPort>,
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
        // 1. Initial TaskCreated event
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

        let tasks_root = Path::new(&req.worktree).join(".liberado").join("tasks");
        let mut ledger = TaskLedger::create_in(tasks_root, initial_event)?;
        let record = ledger.project()?;

        // 2. Synthesize prompt
        let prompt = ContinuationContextBuilder::build(&record);

        let run_req = WorkerRunRequest {
            task_id: req.task_id.clone(),
            objective: req.objective.clone(),
            worktree: req.worktree.clone(),
            branch: req.branch.clone(),
            base_ref: req.base_ref.clone(),
            prompt,
            session_id: None,
        };

        // 3. Launch worker
        let handle = self.worker.start(&run_req)?;

        ledger.append(TaskEvent::new(
            format!("evt-{}-started", handle.run_id),
            &req.task_id,
            TaskEventKind::WorkerStarted {
                run_id: handle.run_id.clone(),
                worker_id: self.worker.id().to_string(),
                resumed_session_id: handle.external_session_id.clone(),
            },
        ))?;

        // 4. Collect result
        let result = self.worker.collect(&handle)?;
        append_worker_finished(&mut ledger, &handle, &result)?;

        // 5. Append commits produced
        for commit in &result.commits {
            ledger.append(TaskEvent::new(
                format!("evt-{}-commit", &commit[..commit.len().min(8)]),
                &req.task_id,
                TaskEventKind::CommitProduced {
                    commit_sha: commit.clone(),
                    message: result.summary.clone(),
                    files_changed: result.files_changed.clone(),
                },
            ))?;
        }

        Ok((ledger, result))
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

        for commit in &result.commits {
            ledger.append(TaskEvent::new(
                format!("evt-{}-repair-commit", &commit[..commit.len().min(8)]),
                &updated_record.task_id,
                TaskEventKind::CommitProduced {
                    commit_sha: commit.clone(),
                    message: result.summary.clone(),
                    files_changed: result.files_changed.clone(),
                },
            ))?;
        }

        Ok(result)
    }
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
