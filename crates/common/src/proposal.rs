//! Proposals — the human-in-the-loop boundary (Decision 11).
//!
//! A `Proposal` is a structured vault artifact written to `proposals/`. High-consequence
//! actions (external comms, irreversible deletes, anything touching `Sensitive`/`FamilyShared`,
//! any write to a `proposal_only` zone) emit one instead of acting. Approval closes through the
//! same machinery: the user approves via the TUI *or* by editing `status: approved` in
//! Obsidian; the daemon picks up that human write and executes the action with the proposal's
//! `correlation_id`. The conservative default is "propose, don't act."

use std::fmt::Write as _;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;

use crate::capability::CapabilitySet;
use crate::dispatch::ToolCall;

/// The fence that separates YAML frontmatter from the note body. A proposal note is exactly one
/// fenced block at the top followed by the human-readable body.
const FRONTMATTER_FENCE: &str = "---";

/// The directory (relative to a vault root) proposal notes live under. Every consumer that reads,
/// writes, or watches proposal files agrees on this one name — `liberado-daemon`,
/// `liberado-telegram-approvals`, and `liberado-executor`'s `RiskGatedToolRuntime` each used to
/// declare their own private copy of the same literal (`docs/roadmap/hygiene-audit-2026-07-05.md`),
/// with only a doc comment (not the compiler) keeping them in agreement.
pub const PROPOSALS_DIR: &str = "proposals";

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
    /// Hand a goal off to a narrowly-scoped subagent — mirrors `DispatchAction::DispatchSubagent`
    /// minus `correlation_id` (the proposal's own `correlation_id` already ties writes back to
    /// this proposal) and `artifact_target`/`model` (not yet consulted by the live execution path
    /// either, so there is nothing yet to preserve for the approved path to honor). Unlike
    /// `ToolCalls`, what was "approved" here is the goal + scoping, not specific tool calls — the
    /// subagent still decides its own calls adaptively on execution, so the approved run stays
    /// runtime-gated the same way a live `DispatchSubagent` is.
    Subagent {
        goal: String,
        #[serde(default)]
        capabilities: CapabilitySet,
        #[serde(default)]
        allowed_mcps: Vec<String>,
        #[serde(default)]
        success_criteria: Vec<String>,
    },
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
    /// Which agent/hook produced this proposal.
    pub source: String,
    pub proposed_action: ProposedAction,
    pub rationale: String,
    pub status: ProposalStatus,
    pub created: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<DateTime<Utc>>,
    /// Which named dispatcher/executor pool (Decision 18 checkpoint #3) proposed this — so
    /// approval executes it via the *same* pool's authority it was proposed under, never a
    /// different (possibly broader) one. `None` routes to the always-present `"default"` pool,
    /// including every proposal written before pools existed. `#[serde(default)]` so an old note
    /// still parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// HMAC-SHA256 (hex-encoded) over `id`/`correlation_id`/`source`/`proposed_action`/`pool`/
    /// `created`, computed by a [`ProposalSigner`] at creation and checked before an approval
    /// executes.
    /// Deliberately excludes `status`/`expires`, which are meant to change as part of the normal
    /// human-approval workflow. Raises the bar against *careless* tampering with the proposed
    /// action between propose and approve (a bug, an accidental overwrite, an opportunistic script
    /// that doesn't go looking for the signing key) — it is **not** a defense against a co-resident
    /// process with the same filesystem access as the daemon, since the signing key lives in a
    /// plain file that process could also read (see `docs/roadmap/hardening-audit-2026-07-02.md`
    /// item 1 for why that requires a different, larger fix). `#[serde(default)]` so a proposal
    /// note written before this field existed still parses — and then correctly fails verification,
    /// since an empty value never matches a real signature.
    #[serde(default)]
    pub integrity: String,
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
            // Routes to the "default" pool unless the caller sets it explicitly afterward (see
            // `pool`'s own doc comment) — callers that don't know about pools at all (most
            // existing call sites, and every proposal note written before pools existed) get
            // today's exact single-pool behavior.
            pool: None,
            // Unsigned until a `ProposalSigner::sign` call sets it — every real production
            // proposal-creation site signs before writing the note; tests that don't care about
            // integrity checking simply never call `execute_approved`/`handle_proposal_change` on
            // an unsigned proposal.
            integrity: String::new(),
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

/// A [`Proposal`] whose `integrity` field is guaranteed to have been computed by a real
/// [`ProposalSigner`] — the only way to construct one is [`ProposalSigner::sign`]. Every
/// proposal-writing helper (`Daemon::write_proposal`, `ChatSessions::write_chat_proposal`,
/// `RiskGatedToolRuntime::write_proposal`) takes a `&SignedProposal`, not a `&Proposal`, so a future
/// call site that forgot to sign is a compile error instead of a proposal that silently fails
/// verification later, discovered only at approval time
/// (`docs/roadmap/hygiene-audit-2026-07-05.md`).
///
/// Deliberately exposes only immutable access ([`Deref`](std::ops::Deref)) plus a narrow
/// [`set_status`](Self::set_status) — not a general `DerefMut`, which would let a caller mutate a
/// signed field after the fact and silently invalidate the signature (exactly the bug class this
/// type exists to prevent). `status`/`expires` are the two fields `ProposalSigner::compute`
/// deliberately excludes (see [`Proposal::integrity`]'s doc comment), so changing them never
/// invalidates the signature and needs no re-sign.
#[derive(Debug, Clone, PartialEq)]
pub struct SignedProposal(Proposal);

impl SignedProposal {
    /// Borrow the wrapped proposal.
    pub fn as_proposal(&self) -> &Proposal {
        &self.0
    }

    /// Consume, discarding the "signed" guarantee — for a caller that genuinely needs an owned
    /// `Proposal` (e.g. to store it in a place typed for the general case).
    pub fn into_proposal(self) -> Proposal {
        self.0
    }

    /// Flip `status` in place — see this type's doc comment for why that's safe without re-signing.
    pub fn set_status(&mut self, status: ProposalStatus) {
        self.0.status = status;
    }
}

impl std::ops::Deref for SignedProposal {
    type Target = Proposal;
    fn deref(&self) -> &Proposal {
        &self.0
    }
}

/// Signs and verifies a [`Proposal`]'s `integrity` field with a shared key — see that field's doc
/// comment for exactly what this does and doesn't defend against. Cheap to clone (the key is
/// reference-counted), so one signer loaded at boot can be threaded to every proposal-creation and
/// approval-checking site without re-reading the key each time.
#[derive(Clone)]
pub struct ProposalSigner {
    key: Arc<[u8]>,
}

impl ProposalSigner {
    /// Build a signer from raw key bytes (e.g. loaded from a persisted key file).
    pub fn new(key: impl Into<Arc<[u8]>>) -> Self {
        Self { key: key.into() }
    }

    /// A signer backed by a fresh, cryptographically random key — for callers with no persisted key
    /// configured (e.g. tests, or a fallback default). A proposal signed with an ephemeral key will
    /// never verify against a different signer instance, including a fresh one built after a
    /// restart with no persisted key.
    pub fn random() -> Self {
        use rand::RngCore;
        let mut key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        Self { key: key.into() }
    }

    /// Sign `proposal`, computing its `integrity` field, and wrap it as a [`SignedProposal`] — the
    /// only way to construct one. Call once at creation, before writing.
    pub fn sign(&self, mut proposal: Proposal) -> SignedProposal {
        proposal.integrity = self.compute(&proposal);
        SignedProposal(proposal)
    }

    /// Whether `proposal`'s `integrity` field matches what this signer computes for its current
    /// immutable fields. `false` for a missing, empty, tampered, or wholesale-forged value.
    pub fn verify(&self, proposal: &Proposal) -> bool {
        !proposal.integrity.is_empty() && self.compute(proposal) == proposal.integrity
    }

    /// HMAC-SHA256 over the proposal's immutable fields, hex-encoded. Deliberately excludes
    /// `status`/`expires` — see `Proposal::integrity`'s doc comment.
    fn compute(&self, proposal: &Proposal) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.key)
            .expect("HMAC accepts a key of any length");
        mac.update(proposal.id.as_bytes());
        mac.update(b"\0");
        mac.update(proposal.correlation_id.as_bytes());
        mac.update(b"\0");
        mac.update(proposal.source.as_bytes());
        mac.update(b"\0");
        let action_json = serde_json::to_vec(&proposal.proposed_action)
            .expect("ProposedAction serializes to JSON");
        mac.update(&action_json);
        mac.update(b"\0");
        mac.update(proposal.pool.as_deref().unwrap_or("").as_bytes());
        mac.update(b"\0");
        mac.update(proposal.created.to_rfc3339().as_bytes());
        hex_encode(&mac.finalize().into_bytes())
    }
}

/// Encode `bytes` as lowercase hex, without pulling in a dedicated hex crate for one call site.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").expect("writing to a String cannot fail");
    }
    s
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
            ProposedAction::Subagent {
                goal, allowed_mcps, ..
            } => {
                let mcps = allowed_mcps.join(", ");
                format!("dispatch a subagent for: {goal} (mcps: {mcps})")
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
    fn note_round_trips_subagent() {
        let p = Proposal::pending(
            "prop-sub-1",
            "prop-sub-1",
            "liberado",
            ProposedAction::Subagent {
                goal: "review recent decisions".into(),
                capabilities: CapabilitySet::from_iter([
                    crate::capability::Capability::ExecuteMcp("decisions-mcp".into()),
                ]),
                allowed_mcps: vec!["decisions-mcp".into()],
                success_criteria: vec!["a review note exists".into()],
            },
            "Open-ended and touches an external-consequence MCP",
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
            "decisions-hook",
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
            "decisions-hook",
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

    fn sample_proposal() -> Proposal {
        Proposal::pending(
            "prop-sig",
            "corr-sig",
            "liberado",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "email:send".into(),
                args: serde_json::json!({ "to": "boss@example.com" }),
            }]),
            "rationale",
        )
    }

    #[test]
    fn signed_proposal_verifies() {
        let signer = ProposalSigner::random();
        let p = sample_proposal();
        assert!(!signer.verify(&p), "unsigned proposal must not verify");
        let signed = signer.sign(p);
        assert!(!signed.integrity.is_empty());
        assert!(signer.verify(signed.as_proposal()));
    }

    #[test]
    fn signature_survives_a_note_round_trip() {
        let signer = ProposalSigner::random();
        let signed = signer.sign(sample_proposal());
        let back = Proposal::from_note(&signed.to_note()).unwrap();
        assert!(signer.verify(&back));
    }

    #[test]
    fn tampered_proposed_action_fails_verification() {
        let signer = ProposalSigner::random();
        let mut p = signer.sign(sample_proposal()).into_proposal();
        // Someone edits the action between propose and approve (directly, or via a note round-trip
        // that a co-resident process rewrote) — the args changed, everything else the same.
        p.proposed_action = ProposedAction::ToolCalls(vec![ToolCall {
            tool: "email:send".into(),
            args: serde_json::json!({ "to": "attacker@example.com" }),
        }]);
        assert!(
            !signer.verify(&p),
            "a tampered proposed_action must fail verification"
        );
    }

    #[test]
    fn tampered_pool_fails_verification() {
        // `pool` decides *which authority* an approved proposal executes with (Decision 18
        // checkpoint #3) — retagging a proposal from a restricted pool onto a broader one would be
        // a real privilege escalation, so it must be as tamper-evident as `proposed_action`.
        let signer = ProposalSigner::random();
        let mut p = sample_proposal();
        p.pool = Some("restricted".into());
        let mut p = signer.sign(p).into_proposal();
        p.pool = Some("default".into());
        assert!(!signer.verify(&p), "a tampered pool must fail verification");
    }

    #[test]
    fn approving_status_alone_does_not_invalidate_the_signature() {
        // status is deliberately excluded from the signed fields — flipping pending -> approved is
        // the normal, expected human-approval edit and must not itself break verification.
        let signer = ProposalSigner::random();
        let mut signed = signer.sign(sample_proposal());
        signed.set_status(ProposalStatus::Approved);
        assert!(signer.verify(&signed));
    }

    #[test]
    fn wholesale_fabricated_proposal_fails_verification() {
        // No signer ever touched this proposal — the shape a co-resident process writing a brand
        // new file (rather than editing an existing legitimate one) would produce.
        let signer = ProposalSigner::random();
        let forged = sample_proposal();
        assert!(!signer.verify(&forged));
    }

    #[test]
    fn different_signer_instances_do_not_cross_verify() {
        // Two signers with independently random keys — simulates a proposal signed in one process
        // lifetime being checked against a different, unrelated key.
        let signer_a = ProposalSigner::random();
        let signer_b = ProposalSigner::random();
        let signed = signer_a.sign(sample_proposal());
        assert!(!signer_b.verify(&signed));
    }

    #[test]
    fn same_key_bytes_verify_across_signer_instances() {
        // A persisted key loaded into two separate ProposalSigner::new(..) calls (e.g. across a
        // process restart) must still cross-verify — proves verification depends on the key, not
        // instance identity.
        let key: Arc<[u8]> = vec![7u8; 32].into();
        let signer_1 = ProposalSigner::new(key.clone());
        let signer_2 = ProposalSigner::new(key);
        let signed = signer_1.sign(sample_proposal());
        assert!(signer_2.verify(&signed));
    }
}
