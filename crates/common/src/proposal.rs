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
use thiserror::Error;

use crate::dispatch::ToolCall;

/// The fence that separates YAML frontmatter from the note body. A proposal note is exactly one
/// fenced block at the top followed by the human-readable body.
const FRONTMATTER_FENCE: &str = "---";

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

    /// Render this proposal as a Markdown note: YAML frontmatter (the full struct, so `status` is
    /// editable in Obsidian and the daemon can parse the action back) + a human-readable body. The
    /// body is for the human — `from_note` reads only the frontmatter, so editing it is harmless.
    pub fn to_note(&self) -> String {
        // serde_yaml emits a trailing newline; the struct serializes infallibly (all fields are
        // plain serde types), so the unwrap can't trip on real data.
        let yaml = serde_yaml::to_string(self).expect("Proposal serializes to YAML");
        format!(
            "{fence}\n{yaml}{fence}\n\n# Proposal: {id}\n\n{rationale}\n\n**Proposed action:** {action}\n\nTo approve, change `status: pending` to `status: approved` above (or use the TUI).\n",
            fence = FRONTMATTER_FENCE,
            id = self.id,
            rationale = self.rationale,
            action = self.proposed_action.summary(),
        )
    }

    /// Parse a proposal note (read its frontmatter). Ignores the body — a human editing the
    /// `status:` line in Obsidian is exactly how approval flows back, so only the frontmatter is
    /// authoritative.
    pub fn from_note(content: &str) -> Result<Proposal, ProposalNoteError> {
        let frontmatter =
            extract_frontmatter(content).ok_or(ProposalNoteError::MissingFrontmatter)?;
        Ok(serde_yaml::from_str(frontmatter)?)
    }
}

impl ProposedAction {
    /// A one-line human summary of the action, for the note body. Not parsed back — `from_note`
    /// reconstructs the structured action from the frontmatter, not this text.
    pub fn summary(&self) -> String {
        match self {
            ProposedAction::ToolCalls(calls) => {
                let tools = calls
                    .iter()
                    .map(|c| c.tool.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("run {} tool call(s): {tools}", calls.len())
            }
            ProposedAction::VaultWrite { path, .. } => format!("write vault note `{path}`"),
            ProposedAction::External { description } => format!("external action: {description}"),
            ProposedAction::Other(value) => format!("other action: {value}"),
        }
    }
}

/// Split out the YAML between the leading `---` fences. Returns `None` when the note has no
/// frontmatter block (so the caller reports [`ProposalNoteError::MissingFrontmatter`]).
fn extract_frontmatter(content: &str) -> Option<&str> {
    let rest = content.strip_prefix(FRONTMATTER_FENCE)?;
    // Skip to the end of the opening fence line, then find the closing fence on its own line.
    let after_open = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    let close = after_open.find(&format!("\n{FRONTMATTER_FENCE}"))?;
    Some(&after_open[..close])
}

/// Errors from parsing a proposal note back into a [`Proposal`].
#[derive(Debug, Error)]
pub enum ProposalNoteError {
    #[error("proposal note has no YAML frontmatter")]
    MissingFrontmatter,
    #[error("malformed proposal frontmatter: {0}")]
    Yaml(#[from] serde_yaml::Error),
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
    fn note_round_trips_tool_calls() {
        let p = Proposal::pending(
            "vault-change:inbox/x.md:abc",
            "vault-change:inbox/x.md:abc",
            "liberado",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "email:send".into(),
                args: serde_json::json!({ "to": "boss@example.com" }),
            }]),
            "The note asks to email the boss",
        );
        let back = Proposal::from_note(&p.to_note()).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.status, ProposalStatus::Pending);
    }

    #[test]
    fn note_round_trips_external() {
        let p = Proposal::pending(
            "prop-2",
            "review-2026-06-21",
            "decisions-acp",
            ProposedAction::External {
                description: "Add family calendar event".into(),
            },
            "Detected a schedulable item",
        );
        assert_eq!(Proposal::from_note(&p.to_note()).unwrap(), p);
    }

    #[test]
    fn human_approval_edit_parses() {
        // A human flips `status: pending` to `status: approved` in the frontmatter text — exactly
        // how approval flows back from Obsidian. The parsed status must reflect the edit.
        let p = Proposal::pending(
            "prop-3",
            "c3",
            "liberado",
            ProposedAction::External {
                description: "Send the email".into(),
            },
            "rationale",
        );
        let approved = p.to_note().replace("status: pending", "status: approved");
        let back = Proposal::from_note(&approved).unwrap();
        assert_eq!(back.status, ProposalStatus::Approved);
    }

    #[test]
    fn missing_frontmatter_is_an_error() {
        assert!(matches!(
            Proposal::from_note("# just a body, no frontmatter"),
            Err(ProposalNoteError::MissingFrontmatter)
        ));
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
