//! Coding-worker control plane domain models, task ledger, and continuation synthesizer.
//!
//! Liberado operates coding agents as interchangeable workers across task lifetimes.
//! The control plane owns the durable append-only task ledger, lifecycle state machine,
//! worker port interface, and context synthesis engine.
//!
//! See `docs/future-work/coding-worker-control-plane.md` for the architectural specification.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

mod config;
pub mod opencode;
pub mod supervisor;

pub use config::{ControlPlaneConfig, NATIVE_WORKER_ID, WorkerAdapterConfig};
pub use opencode::{OpenCodeWorker, OpenCodeWorkerConfig};
pub use supervisor::ControlPlaneSupervisor;

/// Lifecycle status for an external or native worker process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Running,
    Waiting,
    Completed,
    Failed,
}

/// Overall lifecycle status of a task managed by the control plane.
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

/// Errors from the worker control plane and ledger.
#[derive(Debug, Error)]
pub enum ControlPlaneError {
    #[error("empty event history cannot form a task record")]
    EmptyHistory,

    #[error("initial event must be TaskCreated (got {0})")]
    InvalidInitialEvent(String),

    #[error("event task_id '{event_task_id}' does not match ledger task_id '{ledger_task_id}'")]
    TaskIdMismatch {
        event_task_id: String,
        ledger_task_id: String,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("worker protocol error: {0}")]
    Protocol(String),

    #[error("worker run '{0}' was not found")]
    RunNotFound(String),

    #[error("task id '{0}' is not a safe path component")]
    InvalidTaskId(String),

    #[error("duplicate event id '{0}'")]
    DuplicateEventId(String),

    #[error("invalid control-plane config: {0}")]
    InvalidConfig(String),
}

/// Identifier handle for an active or completed worker execution run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHandle {
    pub run_id: String,
    pub worker_id: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_session_id: Option<String>,
    pub worktree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_prompt: Option<String>,
}

impl RunHandle {
    pub fn new(
        run_id: impl Into<String>,
        worker_id: impl Into<String>,
        task_id: impl Into<String>,
        worktree: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            worker_id: worker_id.into(),
            task_id: task_id.into(),
            external_session_id: None,
            worktree: worktree.into(),
            continuation_prompt: None,
        }
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.external_session_id = Some(session_id.into());
        self
    }

    pub fn with_continuation_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.continuation_prompt = Some(prompt.into());
        self
    }
}

/// Request parameters to dispatch a new task via the control plane supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchTaskRequest {
    pub task_id: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub worktree: String,
    pub branch: String,
    pub base_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

impl DispatchTaskRequest {
    pub fn new(
        task_id: impl Into<String>,
        objective: impl Into<String>,
        worktree: impl Into<String>,
        branch: impl Into<String>,
        base_ref: impl Into<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            objective: objective.into(),
            acceptance_criteria: Vec::new(),
            worktree: worktree.into(),
            branch: branch.into(),
            base_ref: base_ref.into(),
            repo: None,
        }
    }

    pub fn with_acceptance_criteria(mut self, criteria: Vec<String>) -> Self {
        self.acceptance_criteria = criteria;
        self
    }

    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }
}

/// Request parameters supplied to initiate a worker execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRunRequest {
    pub task_id: String,
    pub objective: String,
    pub worktree: String,
    pub branch: String,
    pub base_ref: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Normalized result returned by a worker upon completion.
///
/// Ground-truth values (commits, files changed, tests passed) are derived
/// directly from the git worktree and test runner. The summary and next action
/// reflect model-reported claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRunResult {
    pub status: WorkerStatus,
    pub summary: String,
    #[serde(default)]
    pub commits: Vec<String>,
    #[serde(default)]
    pub files_changed: Vec<String>,
    #[serde(default)]
    pub tests_run: u32,
    #[serde(default)]
    pub tests_passed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_next_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_session_id: Option<String>,
}

/// Seam trait implemented by worker backends (native pack, Codex CLI, Claude Code, etc.).
pub trait WorkerPort: Send + Sync {
    /// Identifier for this worker adapter.
    fn id(&self) -> &str;

    /// Launch a worker on a task in the specified workspace worktree.
    fn start(&self, req: &WorkerRunRequest) -> Result<RunHandle, ControlPlaneError>;

    /// Resume a worker following a new task event.
    fn resume(&self, handle: &RunHandle, event: &TaskEvent)
    -> Result<RunHandle, ControlPlaneError>;

    /// Poll the current status of an in-flight run.
    fn status(&self, handle: &RunHandle) -> Result<WorkerStatus, ControlPlaneError>;

    /// Cancel a running execution.
    fn cancel(&self, handle: &RunHandle) -> Result<(), ControlPlaneError>;

    /// Collect the final result once status is terminal (Completed or Failed).
    fn collect(&self, handle: &RunHandle) -> Result<WorkerRunResult, ControlPlaneError>;
}

/// Specific kinds of task lifecycle events recorded in the append-only ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskEventKind {
    TaskCreated {
        objective: String,
        acceptance_criteria: Vec<String>,
        worktree: String,
        branch: String,
        base_ref: String,
        repo: Option<String>,
    },
    WorkerStarted {
        run_id: String,
        worker_id: String,
        resumed_session_id: Option<String>,
    },
    WorkerFinished {
        run_id: String,
        status: WorkerStatus,
        external_session_id: Option<String>,
        blocking_issue: Option<String>,
    },
    CommitProduced {
        commit_sha: String,
        message: String,
        files_changed: Vec<String>,
    },
    TestsPassed {
        tests_run: u32,
    },
    PullRequestOpened {
        pr_number: u64,
        url: String,
    },
    CiFailed {
        run_id: Option<String>,
        failures: Vec<String>,
        failure_log_excerpt: Option<String>,
    },
    WorkerResumed {
        run_id: String,
        worker_id: String,
        reason: String,
    },
    ReviewRejected {
        reviewer: String,
        round: usize,
        diagnosis: String,
    },
    ReviewApproved {
        reviewer: String,
        round: usize,
    },
    CiPassed,
    Escalated {
        reason: String,
    },
}

/// Envelope for one discrete append-only event in a task ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub event_id: String,
    pub task_id: String,
    pub timestamp: DateTime<Utc>,
    pub payload: TaskEventKind,
}

impl TaskEvent {
    pub fn new(
        event_id: impl Into<String>,
        task_id: impl Into<String>,
        payload: TaskEventKind,
    ) -> Self {
        Self {
            event_id: event_id.into(),
            task_id: task_id.into(),
            timestamp: Utc::now(),
            payload,
        }
    }
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
    pub prior_worker: Option<String>,
    pub external_session_id: Option<String>,
    pub current_diagnosis: Option<String>,
    pub commits: Vec<String>,
    pub files_changed: Vec<String>,
    pub failures: Vec<String>,
    pub latest_failure_excerpt: Option<String>,
    pub pull_request_number: Option<u64>,
    pub pull_request_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TaskRecord {
    fn from_task_created(event: &TaskEvent) -> Result<Self, ControlPlaneError> {
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
            prior_worker: None,
            external_session_id: None,
            current_diagnosis: None,
            commits: Vec::new(),
            files_changed: Vec::new(),
            failures: Vec::new(),
            latest_failure_excerpt: None,
            pull_request_number: None,
            pull_request_url: None,
            created_at: event.timestamp,
            updated_at: event.timestamp,
        })
    }

    /// Fold a subsequent event into the active record state.
    pub fn apply(&mut self, event: &TaskEvent) {
        self.updated_at = event.timestamp;
        match &event.payload {
            TaskEventKind::TaskCreated { .. } => {}
            TaskEventKind::WorkerStarted {
                worker_id,
                resumed_session_id,
                ..
            } => {
                self.status = TaskStatus::Running;
                self.prior_worker = Some(worker_id.clone());
                if let Some(session) = resumed_session_id {
                    self.external_session_id = Some(session.clone());
                }
            }
            TaskEventKind::WorkerFinished {
                status,
                external_session_id,
                blocking_issue,
                ..
            } => {
                if let Some(session) = external_session_id {
                    self.external_session_id = Some(session.clone());
                }
                if *status == WorkerStatus::Failed {
                    self.status = TaskStatus::Failed;
                    self.current_diagnosis = blocking_issue.clone();
                }
            }
            TaskEventKind::CommitProduced {
                commit_sha,
                files_changed,
                ..
            } => {
                if !self.commits.contains(commit_sha) {
                    self.commits.push(commit_sha.clone());
                }
                for f in files_changed {
                    if !self.files_changed.contains(f) {
                        self.files_changed.push(f.clone());
                    }
                }
            }
            TaskEventKind::TestsPassed { .. } => {
                self.failures.clear();
                self.latest_failure_excerpt = None;
            }
            TaskEventKind::PullRequestOpened { pr_number, url, .. } => {
                self.pull_request_number = Some(*pr_number);
                self.pull_request_url = Some(url.clone());
                self.status = TaskStatus::NeedsReview;
            }
            TaskEventKind::CiFailed {
                failures,
                failure_log_excerpt,
                ..
            } => {
                self.status = TaskStatus::Repairing;
                self.failures = failures.clone();
                self.latest_failure_excerpt = failure_log_excerpt.clone();
            }
            TaskEventKind::WorkerResumed { worker_id, .. } => {
                self.status = TaskStatus::Running;
                self.prior_worker = Some(worker_id.clone());
            }
            TaskEventKind::ReviewRejected { diagnosis, .. } => {
                self.status = TaskStatus::Repairing;
                self.current_diagnosis = Some(diagnosis.clone());
            }
            TaskEventKind::ReviewApproved { .. } | TaskEventKind::CiPassed => {
                self.status = TaskStatus::Completed;
                self.failures.clear();
                self.latest_failure_excerpt = None;
                self.current_diagnosis = None;
            }
            TaskEventKind::Escalated { reason } => {
                self.status = TaskStatus::Escalated;
                self.current_diagnosis = Some(reason.clone());
            }
        }
    }
}

/// An authoritative append-only ledger for a single task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLedger {
    task_id: String,
    events: Vec<TaskEvent>,
    ledger_path: Option<PathBuf>,
}

impl TaskLedger {
    /// Create a new ledger initialized with a `TaskCreated` event.
    pub fn new(initial_event: TaskEvent) -> Result<Self, ControlPlaneError> {
        if !matches!(initial_event.payload, TaskEventKind::TaskCreated { .. }) {
            return Err(ControlPlaneError::InvalidInitialEvent(format!(
                "{:?}",
                initial_event.payload
            )));
        }
        let task_id = initial_event.task_id.clone();
        Ok(Self {
            task_id,
            events: vec![initial_event],
            ledger_path: None,
        })
    }

    /// Create a disk-backed ledger under `<tasks_root>/<task_id>/ledger.jsonl`.
    pub fn create_in(
        tasks_root: impl AsRef<Path>,
        initial_event: TaskEvent,
    ) -> Result<Self, ControlPlaneError> {
        validate_task_id(&initial_event.task_id)?;
        let task_id = initial_event.task_id.clone();
        let mut ledger = Self::new(initial_event)?;
        let task_dir = tasks_root.as_ref().join(task_id);
        std::fs::create_dir_all(&task_dir)?;
        let ledger_path = task_dir.join("ledger.jsonl");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&ledger_path)?;
        write_event(&mut file, &ledger.events[0])?;
        file.sync_all()?;

        ledger.ledger_path = Some(ledger_path);
        ledger.write_projection_cache()?;
        Ok(ledger)
    }

    /// Append a new event to the ledger history.
    pub fn append(&mut self, event: TaskEvent) -> Result<(), ControlPlaneError> {
        if event.task_id != self.task_id {
            return Err(ControlPlaneError::TaskIdMismatch {
                event_task_id: event.task_id,
                ledger_task_id: self.task_id.clone(),
            });
        }
        if self
            .events
            .iter()
            .any(|existing| existing.event_id == event.event_id)
        {
            return Err(ControlPlaneError::DuplicateEventId(event.event_id));
        }
        if let Some(path) = &self.ledger_path {
            let mut file = OpenOptions::new().append(true).open(path)?;
            write_event(&mut file, &event)?;
            file.sync_all()?;
        }
        self.events.push(event);
        self.write_projection_cache()?;
        Ok(())
    }

    /// Return the immutable slice of all recorded events.
    pub fn events(&self) -> &[TaskEvent] {
        &self.events
    }

    /// Project the events into a unified `TaskRecord`.
    pub fn project(&self) -> Result<TaskRecord, ControlPlaneError> {
        let first = self.events.first().ok_or(ControlPlaneError::EmptyHistory)?;
        let mut record = TaskRecord::from_task_created(first)?;

        for event in &self.events[1..] {
            record.apply(event);
        }

        Ok(record)
    }

    /// Serialize the append-only ledger to a JSONL writer.
    pub fn write_to_writer(&self, mut writer: impl Write) -> Result<(), std::io::Error> {
        for event in &self.events {
            let json = serde_json::to_string(event)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            writeln!(writer, "{json}")?;
        }
        Ok(())
    }

    /// Deserialize a ledger from a JSONL reader.
    pub fn load_from_reader(reader: impl std::io::Read) -> Result<Self, ControlPlaneError> {
        Self::load(reader, None)
    }

    /// Load a disk-backed ledger and continue append-flushed persistence.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ControlPlaneError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        Self::load(file, Some(path))
    }

    fn load(
        reader: impl std::io::Read,
        ledger_path: Option<PathBuf>,
    ) -> Result<Self, ControlPlaneError> {
        let buf = BufReader::new(reader);
        let mut events = Vec::new();
        for line in buf.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let event: TaskEvent = serde_json::from_str(trimmed)?;
            events.push(event);
        }

        let first = events.first().ok_or(ControlPlaneError::EmptyHistory)?;
        let task_id = first.task_id.clone();

        for event in &events {
            if event.task_id != task_id {
                return Err(ControlPlaneError::TaskIdMismatch {
                    event_task_id: event.task_id.clone(),
                    ledger_task_id: task_id,
                });
            }
        }

        if !matches!(first.payload, TaskEventKind::TaskCreated { .. }) {
            return Err(ControlPlaneError::InvalidInitialEvent(format!(
                "{:?}",
                first.payload
            )));
        }

        let mut seen = std::collections::HashSet::new();
        for event in &events {
            if !seen.insert(&event.event_id) {
                return Err(ControlPlaneError::DuplicateEventId(event.event_id.clone()));
            }
        }

        Ok(Self {
            task_id,
            events,
            ledger_path,
        })
    }

    fn write_projection_cache(&self) -> Result<(), ControlPlaneError> {
        let Some(ledger_path) = &self.ledger_path else {
            return Ok(());
        };
        let task_path = ledger_path.with_file_name("task.json");
        let bytes = serde_json::to_vec_pretty(&self.project()?)?;
        std::fs::write(task_path, bytes)?;
        Ok(())
    }
}

fn validate_task_id(task_id: &str) -> Result<(), ControlPlaneError> {
    let path = Path::new(task_id);
    let mut components = path.components();
    let is_one_normal_component =
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none()
            && !task_id.trim().is_empty();
    if is_one_normal_component {
        Ok(())
    } else {
        Err(ControlPlaneError::InvalidTaskId(task_id.to_string()))
    }
}

fn write_event(writer: &mut impl Write, event: &TaskEvent) -> Result<(), ControlPlaneError> {
    serde_json::to_writer(&mut *writer, event)?;
    writer.write_all(b"\n")?;
    Ok(())
}

/// Synthesizes structured markdown prompts for continuation across worker boundaries.
pub struct ContinuationContextBuilder;

impl ContinuationContextBuilder {
    /// Synthesize a normalized continuation prompt from a task projection.
    pub fn build(record: &TaskRecord) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "You are continuing work on task `{}`.\n\n",
            record.task_id
        ));

        out.push_str("## Objective\n");
        out.push_str(&format!("{}\n\n", record.objective.trim()));

        if !record.acceptance_criteria.is_empty() {
            out.push_str("## Acceptance Criteria\n");
            for ac in &record.acceptance_criteria {
                out.push_str(&format!("- {}\n", ac.trim()));
            }
            out.push('\n');
        }

        out.push_str("## Worktree State\n");
        out.push_str(&format!("- Worktree: `{}`\n", record.worktree));
        out.push_str(&format!("- Branch: `{}`\n", record.branch));
        out.push_str(&format!("- Base ref: `{}`\n", record.base_ref));

        if !record.commits.is_empty() {
            out.push_str("- Existing commits on branch:\n");
            for sha in &record.commits {
                out.push_str(&format!("  - `{sha}`\n"));
            }
        }
        out.push('\n');

        if !record.failures.is_empty() {
            out.push_str("## Failures\n");
            out.push_str("The following checks or tests failed:\n");
            for f in &record.failures {
                out.push_str(&format!("- `{f}`\n"));
            }
            out.push('\n');
        }

        if let Some(excerpt) = &record.latest_failure_excerpt {
            out.push_str("### Failure Log Excerpt\n```text\n");
            out.push_str(excerpt.trim());
            out.push_str("\n```\n\n");
        }

        if let Some(diagnosis) = &record.current_diagnosis {
            out.push_str("## Review Diagnosis\n");
            out.push_str(&format!("{}\n\n", diagnosis.trim()));
        }

        out.push_str("## Instructions\n");
        out.push_str("1. Reproduce any reported failure in the worktree before modifying code.\n");
        out.push_str("2. Address the defect directly without refactoring unrelated modules.\n");
        out.push_str("3. Verify that the acceptance criteria are met and tests pass.\n");
        out.push_str("4. Commit your changes with a clear description.\n");

        out
    }
}
