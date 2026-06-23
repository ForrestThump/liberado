//! Proposals — the human-in-the-loop boundary (Decision 11).
//!
//! A `Proposal` is a structured vault artifact written to `proposals/`. High-consequence
//! actions (external comms, irreversible deletes, anything touching `Sensitive`/`FamilyShared`,
//! any write to a `proposal_only` zone) emit one instead of acting. Approval closes through the
//! same machinery: the user approves via the TUI *or* by editing `status: approved` in
//! Obsidian; the daemon picks up that human write and executes the action with the proposal's
//! `correlation_id`. The conservative default is "propose, don't act."

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dispatch::ToolCall;

/// Lifecycle state of a proposal. Lives in the note's frontmatter so it is editable from
/// Obsidian. `Done` marks a proposal whose action has been executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
    Done,
}

impl ProposalStatus {
    /// Whether the proposed action may now be executed by the daemon.
    pub fn is_actionable(self) -> bool {
        matches!(self, Self::Approved)
    }

    /// Terminal states that must never be (re-)executed.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Rejected | Self::Expired | Self::Done)
    }
}

/// The concrete action a proposal would perform once approved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProposedAction {
    /// Run one or more tool calls.
    ToolCalls(Vec<ToolCall>),
    /// Write/replace a vault note.
    VaultWrite {
        path: String,
        content_summary: String,
    },
    /// An externally-consequential action (send, schedule, call an API). Git cannot revert
    /// these — hence the proposal gate stays exactly here.
    External { description: String },
    /// Anything not yet modeled, carried as raw JSON.
    Other(serde_json::Value),
}

/// A proposal artifact. Serializes to the frontmatter of a note under `proposals/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    /// The originating goal/event — reused as the idempotency key when executed.
    pub correlation_id: String,
    /// Which agent/ACP produced this proposal.
    pub source: String,
    pub proposed_action: ProposedAction,
    pub rationale: String,
    pub status: ProposalStatus,
    pub created: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<DateTime<Utc>>,
}

impl Proposal {
    /// Create a new pending proposal stamped `now`.
    pub fn pending(
        id: impl Into<String>,
        correlation_id: impl Into<String>,
        source: impl Into<String>,
        proposed_action: ProposedAction,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            correlation_id: correlation_id.into(),
            source: source.into(),
            proposed_action,
            rationale: rationale.into(),
            status: ProposalStatus::Pending,
            created: Utc::now(),
            expires: None,
        }
    }

    /// Whether this proposal has expired as of `now` (independent of its stored status).
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires.is_some_and(|e| now >= e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_gating() {
        assert!(ProposalStatus::Approved.is_actionable());
        assert!(!ProposalStatus::Pending.is_actionable());
        assert!(ProposalStatus::Rejected.is_terminal());
        assert!(ProposalStatus::Done.is_terminal());
        assert!(!ProposalStatus::Pending.is_terminal());
    }

    #[test]
    fn pending_proposal_round_trips() {
        let p = Proposal::pending(
            "prop-1",
            "review-2026-06-21",
            "decisions-acp",
            ProposedAction::External {
                description: "Add family calendar event".into(),
            },
            "Detected a schedulable item in a decision note",
        );
        let json = serde_json::to_string(&p).unwrap();
        let back: Proposal = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
        assert_eq!(back.status, ProposalStatus::Pending);
    }
}
