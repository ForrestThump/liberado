//! Projected task record: orthogonal evidence, not a single completion status.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{ControlPlaneError, TaskEvent, TaskEventKind, WorkerStatus};

/// Derived lifecycle view for older callers. Ready is never inferred from CI or review alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Created,
    Running,
    NeedsReview,
    Repairing,
    Completed,
    Blocked,
    Escalated,
    Failed,
}

/// Task-level disposition. Orthogonal to CI, review, and the active run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDisposition {
    #[default]
    Open,
    Ready,
    Blocked,
    Escalated,
    Failed,
}

/// Observed CI evidence for the bound revision. Not a task-completion signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiState {
    #[default]
    Unknown,
    Pending,
    Passed,
    Failed,
}

/// Observed review evidence. Not a task-completion signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    #[default]
    None,
    Requested,
    Approved,
    Rejected,
}

/// Exact head SHA plus separate CI and review evidence for a ready decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyEvidence {
    pub head_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_github_run_id: Option<u64>,
    pub review_round: u32,
}

/// Projected snapshot of a task derived deterministically from its event history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub repo: Option<String>,
    pub worktree: String,
    pub branch: String,
    pub base_ref: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub status: TaskStatus,
    #[serde(default)]
    pub disposition: TaskDisposition,
    #[serde(default)]
    pub ci_state: CiState,
    #[serde(default)]
    pub review_state: ReviewState,
    pub prior_worker: Option<String>,
    pub external_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_run_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<String>,
    pub current_diagnosis: Option<String>,
    pub commits: Vec<String>,
    pub files_changed: Vec<String>,
    pub failures: Vec<String>,
    pub latest_failure_excerpt: Option<String>,
    pub pull_request_number: Option<u64>,
    pub pull_request_url: Option<String>,
    #[serde(default)]
    pub rerun_count: u32,
    #[serde(default)]
    pub repair_count: u32,
    #[serde(default)]
    pub review_round: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_evidence: Option<ReadyEvidence>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TaskRecord {
    pub(super) fn from_task_created(event: &TaskEvent) -> Result<Self, ControlPlaneError> {
        let TaskEventKind::TaskCreated {
            objective,
            acceptance_criteria,
            worktree,
            branch,
            base_ref,
            repo,
        } = &event.payload
        else {
            return Err(ControlPlaneError::InvalidInitialEvent(format!(
                "{:?}",
                event.payload
            )));
        };

        Ok(Self {
            task_id: event.task_id.clone(),
            repo: repo.clone(),
            worktree: worktree.clone(),
            branch: branch.clone(),
            base_ref: base_ref.clone(),
            objective: objective.clone(),
            acceptance_criteria: acceptance_criteria.clone(),
            status: TaskStatus::Created,
            disposition: TaskDisposition::Open,
            ci_state: CiState::Unknown,
            review_state: ReviewState::None,
            prior_worker: None,
            external_session_id: None,
            active_run_id: None,
            goal_id: None,
            head_revision: None,
            github_run_id: None,
            controller: None,
            current_diagnosis: None,
            commits: Vec::new(),
            files_changed: Vec::new(),
            failures: Vec::new(),
            latest_failure_excerpt: None,
            pull_request_number: None,
            pull_request_url: None,
            rerun_count: 0,
            repair_count: 0,
            review_round: 0,
            ready_evidence: None,
            created_at: event.timestamp,
            updated_at: event.timestamp,
        })
    }

    /// Fold a subsequent event into the active record state.
    pub fn apply(&mut self, event: &TaskEvent) {
        self.updated_at = event.timestamp;
        apply_kind(self, &event.payload);
    }

    /// Ready only when a controller bound a head SHA plus separate CI and review evidence.
    pub fn is_pr_ready(&self) -> bool {
        let Some(evidence) = &self.ready_evidence else {
            return false;
        };
        self.disposition == TaskDisposition::Ready
            && self.status == TaskStatus::Completed
            && !evidence.head_sha.is_empty()
            && evidence.ci_github_run_id.is_some()
            && evidence.review_round > 0
            && self.ci_state == CiState::Passed
            && self.review_state == ReviewState::Approved
    }
}

fn apply_kind(record: &mut TaskRecord, kind: &TaskEventKind) {
    if apply_worker(record, kind) || apply_repo(record, kind) || apply_evidence(record, kind) {
        return;
    }
    apply_decision(record, kind);
}

fn apply_worker(record: &mut TaskRecord, kind: &TaskEventKind) -> bool {
    match kind {
        TaskEventKind::WorkerStarted {
            run_id,
            worker_id,
            resumed_session_id,
        } => {
            record.status = TaskStatus::Running;
            record.prior_worker = Some(worker_id.clone());
            record.active_run_id = Some(run_id.clone());
            if let Some(session) = resumed_session_id {
                record.external_session_id = Some(session.clone());
            }
            true
        }
        TaskEventKind::WorkerFinished {
            status,
            external_session_id,
            blocking_issue,
            ..
        } => {
            record.active_run_id = None;
            if let Some(session) = external_session_id {
                record.external_session_id = Some(session.clone());
            }
            if *status == WorkerStatus::Failed {
                record.status = TaskStatus::Failed;
                record.disposition = TaskDisposition::Failed;
                record.current_diagnosis = blocking_issue.clone();
            }
            true
        }
        TaskEventKind::WorkerResumed {
            run_id, worker_id, ..
        } => {
            record.status = TaskStatus::Running;
            record.prior_worker = Some(worker_id.clone());
            record.active_run_id = Some(run_id.clone());
            true
        }
        TaskEventKind::CommitProduced {
            commit_sha,
            files_changed,
            ..
        } => {
            if !record.commits.contains(commit_sha) {
                record.commits.push(commit_sha.clone());
            }
            for path in files_changed {
                if !record.files_changed.contains(path) {
                    record.files_changed.push(path.clone());
                }
            }
            true
        }
        _ => false,
    }
}

fn apply_repo(record: &mut TaskRecord, kind: &TaskEventKind) -> bool {
    match kind {
        TaskEventKind::TaskCreated { .. } => true,
        TaskEventKind::PullRequestOpened { pr_number, url } => {
            record.pull_request_number = Some(*pr_number);
            record.pull_request_url = Some(url.clone());
            if record.disposition == TaskDisposition::Open {
                record.status = TaskStatus::NeedsReview;
            }
            true
        }
        TaskEventKind::HeadRevisionObserved { sha } => {
            record.head_revision = Some(sha.clone());
            true
        }
        TaskEventKind::ControllerLeaseClaimed { controller } => {
            record.controller = Some(controller.clone());
            true
        }
        TaskEventKind::GoalLinked { goal_id } => {
            record.goal_id = Some(goal_id.clone());
            true
        }
        _ => false,
    }
}

fn apply_evidence(record: &mut TaskRecord, kind: &TaskEventKind) -> bool {
    match kind {
        TaskEventKind::TestsPassed { .. } => {
            record.failures.clear();
            record.latest_failure_excerpt = None;
            true
        }
        TaskEventKind::CiFailed {
            failures,
            failure_log_excerpt,
            run_id,
        } => {
            record.status = TaskStatus::Repairing;
            record.ci_state = CiState::Failed;
            record.failures = failures.clone();
            record.latest_failure_excerpt = failure_log_excerpt.clone();
            if let Some(run) = run_id
                && let Ok(id) = run.parse::<u64>()
            {
                record.github_run_id = Some(id);
            }
            true
        }
        TaskEventKind::CiPassed => {
            record.ci_state = CiState::Passed;
            record.failures.clear();
            record.latest_failure_excerpt = None;
            true
        }
        TaskEventKind::CiObserved {
            github_run_id,
            head_sha,
            state,
            failures,
        } => {
            apply_ci_observed(record, *github_run_id, head_sha.as_deref(), state, failures);
            true
        }
        TaskEventKind::ReviewRejected {
            diagnosis, round, ..
        } => {
            record.status = TaskStatus::Repairing;
            record.review_state = ReviewState::Rejected;
            record.review_round = (*round).max(1) as u32;
            record.current_diagnosis = Some(diagnosis.clone());
            true
        }
        TaskEventKind::ReviewApproved { round, .. } => {
            record.review_state = ReviewState::Approved;
            record.review_round = (*round).max(1) as u32;
            record.current_diagnosis = None;
            true
        }
        _ => false,
    }
}

fn apply_ci_observed(
    record: &mut TaskRecord,
    github_run_id: Option<u64>,
    head_sha: Option<&str>,
    state: &str,
    failures: &[String],
) {
    record.github_run_id = github_run_id.or(record.github_run_id);
    if let Some(sha) = head_sha {
        record.head_revision = Some(sha.to_string());
    }
    record.ci_state = match state {
        "success" | "passed" => {
            record.failures.clear();
            record.latest_failure_excerpt = None;
            CiState::Passed
        }
        "failure" | "error" | "cancelled" | "timed_out" => {
            if !failures.is_empty() {
                record.failures = failures.to_vec();
            }
            CiState::Failed
        }
        "pending" | "queued" | "in_progress" => CiState::Pending,
        _ => record.ci_state,
    };
}

fn apply_decision(record: &mut TaskRecord, kind: &TaskEventKind) {
    match kind {
        TaskEventKind::Escalated { reason } => {
            record.status = TaskStatus::Escalated;
            record.disposition = TaskDisposition::Escalated;
            record.current_diagnosis = Some(reason.clone());
        }
        TaskEventKind::RerunDecided { github_run_id } => {
            record.rerun_count = record.rerun_count.saturating_add(1);
            record.github_run_id = github_run_id.or(record.github_run_id);
        }
        TaskEventKind::RepairRequested { goal_id, reason } => {
            record.repair_count = record.repair_count.saturating_add(1);
            record.status = TaskStatus::Repairing;
            record.current_diagnosis = Some(reason.clone());
            if let Some(id) = goal_id {
                record.goal_id = Some(id.clone());
            }
        }
        TaskEventKind::ReviewRequested { round, goal_id } => {
            record.review_state = ReviewState::Requested;
            record.review_round = (*round).max(1) as u32;
            if let Some(id) = goal_id {
                record.goal_id = Some(id.clone());
            }
        }
        TaskEventKind::ReadyDecided {
            head_sha,
            ci_github_run_id,
            review_round,
        } => {
            record.disposition = TaskDisposition::Ready;
            record.status = TaskStatus::Completed;
            record.head_revision = Some(head_sha.clone());
            record.github_run_id = ci_github_run_id.or(record.github_run_id);
            record.review_round = (*review_round).max(record.review_round);
            if ci_github_run_id.is_some() {
                record.ci_state = CiState::Passed;
            }
            record.review_state = ReviewState::Approved;
            record.ready_evidence = Some(ReadyEvidence {
                head_sha: head_sha.clone(),
                ci_github_run_id: *ci_github_run_id,
                review_round: *review_round,
            });
            record.failures.clear();
            record.latest_failure_excerpt = None;
            record.current_diagnosis = None;
        }
        TaskEventKind::BlockedDecided { reason } => {
            record.disposition = TaskDisposition::Blocked;
            record.status = TaskStatus::Blocked;
            record.current_diagnosis = Some(reason.clone());
        }
        _ => {}
    }
}
