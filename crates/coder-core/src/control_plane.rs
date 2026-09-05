//! Coding-worker control plane domain models, task ledger, and continuation synthesizer.
//!
//! Liberado operates coding agents as interchangeable workers across task lifetimes.
//! The control plane owns the durable append-only task ledger, orthogonal evidence
//! dimensions, worker port interface, and context synthesis engine.
//!
//! See `docs/future-work/coding-worker-control-plane.md` for the architectural specification.

use serde::{Deserialize, Serialize};
use thiserror::Error;

mod config;
mod continuation;
mod events;
mod ids;
mod ledger;
pub mod opencode;
mod record;
pub mod supervisor;

pub use config::{ControlPlaneConfig, NATIVE_WORKER_ID, WorkerAdapterConfig};
pub use continuation::ContinuationContextBuilder;
pub use events::{TaskEvent, TaskEventKind};
pub use ids::{
    CONTROLLER_GROK_BOT, CONTROLLER_LIBERADO_SHEPHERD, durable_tasks_root, shepherd_task_id,
    tasks_root_from_worktree,
};
pub use ledger::TaskLedger;
pub use opencode::{OpenCodeWorker, OpenCodeWorkerConfig};
pub use record::{CiState, ReadyEvidence, ReviewState, TaskDisposition, TaskRecord, TaskStatus};
pub use supervisor::{ControlPlaneSupervisor, SupervisedRun};

/// Lifecycle status for an external or native worker process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Running,
    Waiting,
    Completed,
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

    #[error("controller lease held by '{held}' (requested '{requested}')")]
    ControllerLeaseConflict { held: String, requested: String },
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
    /// Durable ledger root. When unset, the supervisor resolves from the worktree lease.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger_root: Option<String>,
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
            ledger_root: None,
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

    pub fn with_ledger_root(mut self, ledger_root: impl Into<String>) -> Self {
        self.ledger_root = Some(ledger_root.into());
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
