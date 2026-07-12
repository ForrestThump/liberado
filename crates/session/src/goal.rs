//! Goal and session status types (domain-neutral).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Which domain pack should run this goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DomainHint {
    /// Default for dogfood / self-improvement coding goals.
    #[default]
    Coding,
    Life,
    /// Explicit pack name for future packs.
    Custom(String),
}

impl DomainHint {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Coding => "coding",
            Self::Life => "life",
            Self::Custom(s) => s.as_str(),
        }
    }
}

/// Input to start a goal session (HTTP body / client contract).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalSpec {
    /// Optional client-supplied id; server generates a ULID when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub description: String,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    #[serde(default)]
    pub domain: DomainHint,
    /// Soft turn budget for the pack (0 = pack default).
    #[serde(default)]
    pub max_turns: u32,
    /// Opaque pack payload (workspace root, vault path, contract JSON, …).
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Lifecycle of one goal session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    BudgetExhausted,
}

impl SessionStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::BudgetExhausted
        )
    }
}

/// How the session ended (surface-friendly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    Succeeded,
    Failed,
    Cancelled,
    BudgetExhausted,
}

/// Pack outcome after a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalResult {
    pub terminal: TerminalKind,
    pub summary: String,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub diagnostics: serde_json::Value,
}

/// Stored session row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalSessionRecord {
    pub id: String,
    pub goal: GoalSpec,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<GoalResult>,
    #[serde(default)]
    pub event_count: usize,
}

impl GoalSessionRecord {
    pub fn new(goal: GoalSpec) -> Self {
        let id = goal
            .id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| Ulid::new().to_string());
        Self {
            id,
            goal,
            status: SessionStatus::Pending,
            created_at: Utc::now(),
            finished_at: None,
            result: None,
            event_count: 0,
        }
    }
}
