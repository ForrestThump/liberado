//! Append-only task events. Command ids make duplicate observations a no-op.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::WorkerStatus;

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
    ControllerLeaseClaimed {
        controller: String,
    },
    HeadRevisionObserved {
        sha: String,
    },
    CiObserved {
        github_run_id: Option<u64>,
        head_sha: Option<String>,
        state: String,
        failures: Vec<String>,
    },
    RerunDecided {
        github_run_id: Option<u64>,
    },
    RepairRequested {
        goal_id: Option<String>,
        reason: String,
    },
    ReviewRequested {
        round: usize,
        goal_id: Option<String>,
    },
    ReadyDecided {
        head_sha: String,
        ci_github_run_id: Option<u64>,
        review_round: u32,
    },
    BlockedDecided {
        reason: String,
    },
    GoalLinked {
        goal_id: String,
    },
}

/// Envelope for one discrete append-only event in a task ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub event_id: String,
    pub task_id: String,
    pub timestamp: DateTime<Utc>,
    pub payload: TaskEventKind,
    /// Caller-supplied idempotency key. A second record with the same key is a no-op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
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
            command_id: None,
        }
    }

    pub fn with_command_id(mut self, command_id: impl Into<String>) -> Self {
        self.command_id = Some(command_id.into());
        self
    }
}
