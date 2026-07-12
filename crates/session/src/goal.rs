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

/// Where a goal session came from — the link back to a parent conversation when a chat turn spawned
/// it (session-focus D2/S4). Separate-but-linked: the session's transcript stays its own, but on
/// terminal its summary can be folded back into `conversation_id`, and `correlation_id` stitches it
/// to the dispatch journal — the same linkage `delegate` already uses. `None` for sessions a human
/// started directly (`POST /api/goals`, `/sessions` switcher).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOrigin {
    /// The parent conversation id (a `conversation-store` ULID, as a string here to keep the
    /// session kernel off that store-tier crate).
    pub conversation_id: String,
    /// The dispatch correlation id that ties this session to the journal entry that spawned it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
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
    /// Kernel idle budget for **interactive** sessions: if the pack blocks on human input this
    /// long with no answer, the session terminates `BudgetExhausted` (an abandoned session must
    /// still die). `None` = wait indefinitely. Non-interactive packs ignore it. This is a kernel
    /// budget, not a pack knob — enforced by [`InputChannel`](crate::InputChannel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_idle_secs: Option<u64>,
    /// Set when a chat turn spawned this session — the link back to the parent conversation for the
    /// offer/return handoff (S4). `None` for human-started sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOrigin>,
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
    /// True while the pack is blocked awaiting human input (interactive sessions). Derived by the
    /// store from `AwaitingInput` / `HumanInput` events, so list/snapshot views can badge sessions
    /// that need a human without scanning the event log.
    #[serde(default)]
    pub awaiting_input: bool,
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
            awaiting_input: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_round_trips_and_is_omitted_when_absent() {
        let with = GoalSpec {
            id: None,
            description: "build a CLI".into(),
            success_criteria: vec![],
            domain: DomainHint::Coding,
            max_turns: 0,
            max_idle_secs: None,
            origin: Some(SessionOrigin {
                conversation_id: "01CONV".into(),
                correlation_id: Some("corr-1".into()),
            }),
            payload: serde_json::json!({}),
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["origin"]["conversation_id"], "01CONV");
        let back: GoalSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back.origin.as_ref().unwrap().conversation_id, "01CONV");

        // Absent origin is skipped in the wire form (backward-compatible with human-started goals).
        let without = GoalSpec { origin: None, ..with };
        let json = serde_json::to_value(&without).unwrap();
        assert!(json.get("origin").is_none(), "origin should be omitted when None");
        let rec = GoalSessionRecord::new(without);
        assert!(rec.goal.origin.is_none());
    }
}
