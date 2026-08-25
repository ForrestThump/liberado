//! Wire contract for LAN delegation (`docs/future-work/delegate-network-plan.md` §5).
//!
//! One delegator machine hands coding tasks to one worker machine over HTTP. Both sides
//! depend only on this crate for the types they exchange, mirroring `chat-client-contract`:
//! DTOs and route constants here, transport nowhere. Delivery is at-least-once; every
//! message that crosses twice (submit, events) carries an idempotency key and receivers
//! deduplicate — the same discipline the vault inbox uses.
//!
//! Nothing in this crate may grow transport dependencies: a client crate stays liftable
//! into any surface (enforced by `layer_rules.rs`, client purity).

use serde::{Deserialize, Serialize};

/// Routes of the worker's control plane. All token-protected; the worker hosts them
/// under one root so reverse-proxy or firewall rules stay a single prefix.
pub mod routes {
    pub const ROOT: &str = "/v1/delegate";
    pub const HEALTH: &str = "/v1/delegate/health";
    pub const TASKS: &str = "/v1/delegate/tasks";

    /// `GET {task}` — status poll (the reconciliation path when SSE is missed).
    pub fn task(task_id: &str) -> String {
        format!("{TASKS}/{task_id}")
    }

    /// `POST {task_answers}` — reply to a question / kickback instruction (D2).
    pub fn task_answers(task_id: &str) -> String {
        format!("{TASKS}/{task_id}/answers")
    }

    /// `POST {task_cancel}` — cooperative stop at the next tool boundary.
    pub fn task_cancel(task_id: &str) -> String {
        format!("{TASKS}/{task_id}/cancel")
    }
}

/// Idempotency key for task submission. A ULID minted by the delegator; the worker's
/// queue is keyed on it, so a replayed submit is a no-op rather than a second run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    /// The short form used in branch names: lowercase, last 6 characters. The tail on
    /// purpose — a ULID's leading characters encode the timestamp, so every task
    /// minted within the same minute shares them and two bursts would collide on one
    /// branch name. The trailing characters are the random section.
    pub fn short(&self) -> String {
        let len = self.0.chars().count();
        let start = len.saturating_sub(6);
        self.0
            .chars()
            .skip(start)
            .collect::<String>()
            .to_ascii_lowercase()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What needs doing. The delegator defines the work *and* its acceptance gates;
/// the worker runs it inside a narrowed grant and cannot lower the bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: TaskId,
    /// Names a coding project on the worker (its own config stack resolves paths/models).
    pub project: String,
    /// OWNER/REPO (or Gitea path); the clone URL is resolved by worker configuration.
    pub repository: String,
    pub base_branch: String,
    /// The full description of what needs doing.
    pub goal: String,
    /// Same shape as `CodingSubtask.success_criteria`.
    #[serde(default)]
    pub success_criteria: Vec<String>,
    pub acceptance: Acceptance,
    #[serde(default)]
    pub budget: TaskBudget,
    #[serde(default)]
    pub grant: TaskGrant,
}

/// Acceptance gates travel in the task (plan §6): the worker must clear all three layers
/// before the delegator ever reviews. D1 carries the shape; D3 wires enforcement.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Acceptance {
    /// Run on the worker before the PR opens (ship bar). Wire mirror of
    /// `coder-sandbox`'s `PreflightStep` — the contract crate cannot reach pack types.
    #[serde(default)]
    pub preflight: Vec<PreflightStepDto>,
    /// Forge CI check names that must pass; verified delegator-side, not trusted to the forge.
    #[serde(default)]
    pub required_checks: Vec<String>,
    /// The diff must not touch these; also shown to the cold-review prompt.
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
}

/// Wire mirror of `coder_sandbox::PreflightStep`. Field-for-field so the mapping is total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightStepDto {
    pub name: String,
    pub run: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

/// Resource ceilings for the run. The executor's own budget machinery kills the loop;
/// `None` fields mean "worker default", not "unbounded forever".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBudget {
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub wall_clock_secs: Option<u64>,
    #[serde(default)]
    pub token_cap: Option<u64>,
}

/// How far the worker may go, narrowed from its own standing grants. The delegator's
/// grant can narrow further, never widen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskGrant {
    /// Branch namespace under `delegate/` the worker must push into.
    #[serde(default)]
    pub branch_namespace: Option<String>,
    #[serde(default)]
    pub max_kickbacks: u32,
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
}

impl Default for TaskGrant {
    fn default() -> Self {
        Self {
            branch_namespace: None,
            max_kickbacks: 2,
            forbidden_paths: Vec::new(),
        }
    }
}

/// Worker → delegator question (D2). Structured options make the delegator's job a
/// choice instead of a research task; `default_option` bounds the stall when nobody answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub correlation_id: String,
    pub task_id: TaskId,
    pub session_id: String,
    /// What is blocking, what was tried.
    pub body: String,
    pub options: Vec<QuestionOption>,
    /// What the worker does if the answer times out.
    pub default_option: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    /// The consequence of choosing this option.
    pub consequence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub question_id: String,
    pub chosen_option: Option<String>,
    pub body: String,
}

/// The kinds of `WorkerEvent`. Stable wire vocabulary: new variants append, never rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Question,
    StatusChanged,
    PrReady,
    Blocked,
}

/// One event on the SSE stream (D2). Payload shape depends on `kind`; consumers match
/// on the kind before unpacking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerEvent {
    pub kind: EventKind,
    pub correlation_id: String,
    pub task_id: TaskId,
    pub payload: serde_json::Value,
}

/// Lifecycle status of a task on the worker. D1 spans Queued → Running → PrOpened /
/// Failed / Cancelled; Blocked arrives with monitor plumbing (D3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum TaskStatus {
    Queued,
    Running,
    PrOpened { url: String },
    Failed { reason: String },
    Cancelled,
}

/// Everything a poll returns. `pr_url` duplicates the PrOpened variant on purpose:
/// surfaces render it without matching on the status enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub spec: TaskSpec,
    pub status: TaskStatus,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    /// RFC 3339 timestamp of the last transition.
    pub updated_at: String,
}

/// Response to `POST /tasks`. `duplicate = true` means the id already existed: the
/// stored record came back unchanged and nothing re-ran (at-least-once delivery).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitOutcome {
    pub record: TaskRecord,
    pub duplicate: bool,
}

/// Liveness + build fingerprint (`GET /health`). The supervisor logs fingerprint
/// mismatches loudly — a dispatched run tests the installed binary, not your working tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerHealth {
    pub status: String,
    pub version: String,
    /// Crate version + git describe, captured at build time.
    pub fingerprint: String,
}

#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct RejectReason {
    pub message: String,
}

impl RejectReason {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two ULIDs minted in the same millisecond share their leading (timestamp)
    /// characters; the short form must still differ because it draws from the random
    /// tail. This is the property that keeps two same-minute tasks off one branch.
    #[test]
    fn short_draws_from_the_random_tail_not_the_timestamp() {
        let a = TaskId("01JGITEAFORGE0000000TEST1".to_string());
        let b = TaskId("01JGITEAFORGE0000000TEST2".to_string());
        assert_ne!(a.short(), b.short());
        assert_eq!(a.short(), "0test1");
        // Stable and lowercase regardless of input case.
        let c = TaskId("01JGITEAFORGE0000000TEST1".to_string());
        assert_eq!(c.short(), "0test1");
    }
}
