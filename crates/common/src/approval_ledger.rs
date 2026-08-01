//! Where a human's approval decision actually lives — outside the vault, out of every agent's reach.
//!
//! # Why this exists
//!
//! A proposal note carries `status: pending|approved|…`, and until now that field *was* the
//! authorisation: the daemon read it and executed. Two facts made that unsafe together.
//!
//! * `proposals/` is declared `agent_writable` in policy, so agent grants include
//!   `Write(Vault("proposals"))`. An agent editing a proposal via an MCP tool is a permitted write.
//! * `status` is deliberately **excluded** from a proposal's integrity signature, because a human
//!   must be able to flip it without invalidating the hash.
//!
//! Each is right on its own. Together they mean the approval field of the approval mechanism was
//! writable by the thing being gated. Observed 2026-08-01: an agent, told by the gate's own message
//! that approval was its to give, read its pending proposal and ran `turbovault:edit_note` against
//! it. The wording that invited that is fixed; the capability it exercised is this module's problem.
//!
//! Provenance could not close it. `WriteProvenance` distinguishes human from agent and rides every
//! vault write, but attribution is a content-hash join against TurboVault's audit log, and **MCP
//! tool writes carry no provenance into that log** — the fallback is the note's front matter, which
//! proposals do not have. An agent's `edit_note` therefore attributes as `External`, which this
//! system defines as *"an external/human edit."* A provenance check would have named the agent the
//! human.
//!
//! # What this is
//!
//! An append-only decision log under `<LIBERADO_DATA_DIR>/`, which no MCP mounts and no tool
//! addresses. The proposal note stays exactly as it is and becomes a **view**: readable, useful,
//! and no longer believed. Editing `status:` — in Obsidian, over Syncthing, or by an agent — stops
//! authorising anything.
//!
//! The boundary is process separation, not a secret. A key held in the daemon's environment is
//! readable by anything that gets shell in the daemon's container, so encryption would protect
//! confidentiality while the threat here is integrity. Keeping the record somewhere the agent has
//! no path to is what buys the property, and it survives tools that do not exist yet — a future
//! subprocess tool defeats a path check and an env-var key, and does not defeat "the record lives
//! where you cannot address it".
//!
//! # Fail-closed
//!
//! No entry means no execution. A missing, unreadable, or corrupt ledger authorises nothing, which
//! is the safe direction: the cost is a proposal that needs re-approving, against the alternative
//! of one that runs because a file could not be read.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// File name under the data dir. Append-only JSONL, one decision per line, same shape as the
/// session store — a decision is a fact that happened, never edited afterwards.
const LEDGER_FILE: &str = "approvals.jsonl";

/// What a human decided about one proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

/// One recorded decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub proposal_id: String,
    pub decision: ApprovalDecision,
    pub at: DateTime<Utc>,
    /// Which authenticated surface recorded it — `"telegram"`, `"tui"`. Audit only; the ledger's
    /// security comes from *where it lives*, not from this string, which a caller chooses freely.
    pub by: String,
}

/// The append-only record of human approval decisions.
///
/// Cheap to construct and clone — it holds a path, not a handle. Reads scan the file, which is fine
/// at the scale this operates: decisions are rare and the file is small. If it ever is not, the
/// shape is the same one the session store already indexes.
#[derive(Debug, Clone)]
pub struct ApprovalLedger {
    path: PathBuf,
}

impl ApprovalLedger {
    /// The ledger under `data_dir` — pass `liberado_config::data_dir()`. Taken as a path rather
    /// than resolved here so this crate keeps its zero workspace dependencies.
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join(LEDGER_FILE),
        }
    }

    /// Where the ledger is written, for logging and tests.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record a decision. Called only by an authenticated surface — the Telegram approve/reject
    /// buttons, the TUI — never from a tool runtime.
    pub async fn record(
        &self,
        proposal_id: &str,
        decision: ApprovalDecision,
        by: &str,
    ) -> std::io::Result<()> {
        let record = ApprovalRecord {
            proposal_id: proposal_id.to_string(),
            decision,
            at: Utc::now(),
            by: by.to_string(),
        };
        let mut line = serde_json::to_string(&record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');

        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // Append, never rewrite: a decision already made must not be alterable by making another.
        use tokio::io::AsyncWriteExt as _;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await
    }

    /// The decision recorded for `proposal_id`, if any.
    ///
    /// The **last** matching entry wins, so a reject following an approve is honoured — the file is
    /// append-only, so changing one's mind means appending, not editing.
    ///
    /// Returns `None` when the ledger is missing or unreadable, and skips lines that do not parse.
    /// Every one of those is the fail-closed direction: no decision found means nothing runs.
    pub async fn decision_for(&self, proposal_id: &str) -> Option<ApprovalDecision> {
        let content = tokio::fs::read_to_string(&self.path).await.ok()?;
        content
            .lines()
            .filter_map(|line| serde_json::from_str::<ApprovalRecord>(line).ok())
            .rfind(|record| record.proposal_id == proposal_id)
            .map(|record| record.decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger(dir: &tempfile::TempDir) -> ApprovalLedger {
        ApprovalLedger::new(dir.path())
    }

    /// The property this module exists for: a proposal nobody approved has no decision, so a note
    /// claiming `status: approved` authorises nothing on its own.
    #[tokio::test]
    async fn an_unapproved_proposal_has_no_decision() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(ledger(&dir).decision_for("prop-1").await, None);
    }

    #[tokio::test]
    async fn a_recorded_decision_is_readable_back() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        l.record("prop-1", ApprovalDecision::Approved, "telegram")
            .await
            .unwrap();
        assert_eq!(
            l.decision_for("prop-1").await,
            Some(ApprovalDecision::Approved)
        );
        // ...and says nothing about any other proposal.
        assert_eq!(l.decision_for("prop-2").await, None);
    }

    /// Append-only means changing your mind appends. The latest entry is the decision in force.
    #[tokio::test]
    async fn the_latest_decision_wins() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        l.record("prop-1", ApprovalDecision::Approved, "telegram")
            .await
            .unwrap();
        l.record("prop-1", ApprovalDecision::Rejected, "telegram")
            .await
            .unwrap();
        assert_eq!(
            l.decision_for("prop-1").await,
            Some(ApprovalDecision::Rejected),
            "a later reject must override an earlier approve, not be shadowed by it"
        );
    }

    /// A corrupt line must not take the ledger down with it, and must not be read as consent.
    #[tokio::test]
    async fn a_corrupt_line_is_skipped_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        l.record("prop-1", ApprovalDecision::Approved, "telegram")
            .await
            .unwrap();
        tokio::fs::write(
            l.path(),
            format!(
                "{{ not json at all\n{}\n",
                tokio::fs::read_to_string(l.path())
                    .await
                    .unwrap()
                    .trim_end()
            ),
        )
        .await
        .unwrap();

        assert_eq!(
            l.decision_for("prop-1").await,
            Some(ApprovalDecision::Approved),
            "a valid decision must survive a neighbouring corrupt line"
        );
        assert_eq!(
            l.decision_for("prop-unknown").await,
            None,
            "and garbage must never resolve to consent for something never decided"
        );
    }
}
