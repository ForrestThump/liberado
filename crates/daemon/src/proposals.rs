//! Proposal write, approval handling, archive, and grant application.

use std::path::Path;

use liberado_common::{DEFAULT_POOL, SignedProposal};
use liberado_orchestrator::Disposition;

use crate::helpers::{archive_outcome_subdir, grant_component_for_pool, slugify};
use crate::types::{DAEMON_SOURCE, Daemon, DaemonError, PROPOSALS_ARCHIVE_DIR, ReactionOutcome};

impl Daemon {
    /// Persist a proposal as a Markdown note under `proposals/`. Tagged with agent provenance for
    /// the daemon's own source, so the resulting change is attributed to us and not re-reacted to.
    /// The proposal's `id` (a correlation id with `:`/`/`) is slugified for the *filename* only —
    /// the authoritative id stays intact in the frontmatter for idempotency.
    pub(crate) async fn write_proposal(
        &self,
        proposal: &SignedProposal,
    ) -> Result<(), DaemonError> {
        let stem = slugify(&proposal.id);
        let path = format!("proposals/{stem}.md");
        let provenance =
            liberado_common::WriteProvenance::agent(DAEMON_SOURCE, &proposal.correlation_id);
        self.vault
            .write(&path, &proposal.to_note(), None, &provenance)
            .await?;

        if let Some(notifier) = &self.notifier {
            let message = format!(
                "Liberado: a new proposal needs your review.\n{}\nSaved at: {path}",
                proposal.rationale
            );
            if let Err(e) = notifier.notify_proposal(&stem, &message).await {
                // Best-effort — see RiskGatedToolRuntime::write_proposal's identical reasoning.
                tracing::warn!(error = %e, "failed to send proposal notification");
            }
        }

        Ok(())
    }

    /// A human edited a note under `proposals/`. If it is an APPROVED, non-expired, non-terminal
    /// proposal, execute its action and flip it to `done`. Anything else (still pending, rejected,
    /// expired, already done, or not a parseable proposal) is observed and left alone.
    pub(crate) async fn handle_proposal_change(
        &self,
        rel_path: &Path,
    ) -> Result<ReactionOutcome, DaemonError> {
        // 1. Read the current content (may have vanished — VaultError propagates).
        let content = self.vault.read(rel_path).await?;

        // 2. Parse. A non-parseable note is just observed (likely a non-proposal file in proposals/,
        //    or a note whose frontmatter was temporarily mangled during an edit).
        let mut proposal = match liberado_common::Proposal::from_note(&content) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(error = %e, "proposals/ change is not a parseable proposal");
                return Ok(ReactionOutcome::Observed);
            }
        };

        // 2.5. Integrity check: detects tampering with the proposal's immutable fields (or a
        //    wholesale-forged proposal with no valid signature at all) between creation and this
        //    edit. This must run before anything else that could execute — a failure is observed
        //    and left alone, never marked done, so it's never silently treated as if it had
        //    legitimately run. See `Proposal::integrity`'s doc comment for what this does and
        //    doesn't defend against.
        if !self.signer.verify(&proposal) {
            tracing::warn!(
                proposal_id = %proposal.id,
                "proposal failed integrity verification — refusing to treat as actionable \
                 (possible tampering)"
            );
            return Ok(ReactionOutcome::Observed);
        }

        // 3. Terminal states are never re-executed (at-most-once journal marker, Decision 6). This
        //    is also where a human deny lands (the Telegram/Obsidian write flips status to
        //    Rejected): observe it, and file the resolved note into the archive so the active dir
        //    doesn't accumulate it. The approve path archives its own note inline (step 7.5) — its
        //    Done write is suppressed and so never re-observes here.
        if proposal.status.is_terminal() {
            tracing::debug!(status = ?proposal.status, "proposal is already terminal");
            self.archive_terminal_proposal(rel_path, &proposal).await;
            return Ok(ReactionOutcome::Observed);
        }

        // 4. Expired proposals are never executed.
        if proposal.is_expired_at(chrono::Utc::now()) {
            tracing::debug!("proposal is expired");
            return Ok(ReactionOutcome::Observed);
        }

        // 5. Only Approved is actionable — the human edited something other than approving.
        if !proposal.status.is_actionable() {
            tracing::debug!(status = ?proposal.status, "proposal is not actionable");
            return Ok(ReactionOutcome::Observed);
        }

        // 6. Execute — via the *same* pool this proposal was proposed under (Decision 18
        //    checkpoint #3), never a different one, so a restricted pool's proposal can never
        //    execute with a different (possibly broader) pool's authority. `Orchestrator::
        //    execute_approved` itself defensively re-checks this too (defense in depth).
        //    An orchestration error is an infra failure and propagates (so it can be retried on
        //    the next watch cycle). We do NOT mark done on failure.
        let pool_name = proposal.pool.as_deref().unwrap_or(DEFAULT_POOL);
        let Some(orch) = self
            .pools
            .get(pool_name)
            .and_then(|pool| pool.orchestrator.as_ref())
        else {
            tracing::warn!(
                pool = pool_name,
                "approved proposal's pool has no orchestrator attached to execute it"
            );
            return Ok(ReactionOutcome::Observed);
        };
        let report = orch.execute_approved(&proposal).await?;

        // 6.5. If this was a permission request, apply the grant the human chose. The call itself
        //     already ran (step 6, human tap = gate); this is only about whether FUTURE calls need
        //     to ask again. Best-effort — a persistence failure never fails the reaction.
        self.apply_approved_grant(&proposal);

        // 7. Mark done and persist. The write carries agent provenance (DAEMON_SOURCE) so
        //    attribution suppresses it — no self-reaction (loop-break, Decision 5).
        proposal.status = liberado_common::ProposalStatus::Done;
        let provenance =
            liberado_common::WriteProvenance::agent(DAEMON_SOURCE, &proposal.correlation_id);
        self.vault
            .write(rel_path, &proposal.to_note(), None, &provenance)
            .await?;

        tracing::info!(
            proposal_id = %proposal.id,
            outcome = ?report.outcome,
            "executed approved proposal and marked done"
        );

        // 7.5. File the now-Done note into the archive so it leaves the active proposals dir. The
        //     move is a suppressed DAEMON_SOURCE write to the excluded archive subtree, so it never
        //     re-observes — this is the *only* place an approved note gets archived (its Done write
        //     above never surfaces to the terminal-observe branch).
        self.archive_terminal_proposal(rel_path, &proposal).await;

        if let Some(notifier) = &self.notifier {
            let message = format!(
                "Liberado: proposal executed.\n{}\nOutcome: {:?}",
                proposal.rationale, report.outcome
            );
            if let Err(e) = notifier.notify(&message).await {
                // Best-effort — the action already ran and was marked done; a failed
                // confirmation just means the human finds out by checking the vault instead.
                tracing::warn!(error = %e, "failed to send proposal-executed notification");
            }
        }

        Ok(ReactionOutcome::Acted(Disposition::Reported(report)))
    }

    /// Best-effort move of a now-terminal proposal note out of the active `proposals/` dir into
    /// `proposals/archive/<outcome>/`, so the active dir doesn't silt up with resolved notes
    /// (Gap 1). The note's frontmatter still records the authoritative status + scope; the folder
    /// split just makes the outcome legible without opening files.
    ///
    /// Safe against re-entry by construction: the move carries `DAEMON_SOURCE` provenance so the
    /// destination write is suppressed by attribution, the source removal is a `FileDeleted` the
    /// watch loop already skips, and `react` excludes the archive subtree outright. A non-terminal
    /// status has no archive home and is left in place.
    ///
    /// Failure is logged and swallowed: the terminal status is already persisted, so a note that
    /// fails to archive is merely left in the active dir — never lost, never re-executed.
    pub(crate) async fn archive_terminal_proposal(
        &self,
        rel_path: &Path,
        proposal: &liberado_common::Proposal,
    ) {
        let Some(outcome) = archive_outcome_subdir(proposal.status) else {
            return; // not terminal — nothing to archive
        };
        let Some(file_name) = rel_path.file_name().and_then(|n| n.to_str()) else {
            tracing::warn!(path = %rel_path.display(), "proposal path has no file name — not archiving");
            return;
        };
        let dest = format!("{PROPOSALS_ARCHIVE_DIR}/{outcome}/{file_name}");
        let provenance =
            liberado_common::WriteProvenance::agent(DAEMON_SOURCE, &proposal.correlation_id);
        match self
            .vault
            .move_note(rel_path, &dest, None, &provenance)
            .await
        {
            Ok(()) => tracing::info!(
                proposal_id = %proposal.id,
                to = %dest,
                "archived terminal proposal out of the active proposals dir"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                from = %rel_path.display(),
                to = %dest,
                "failed to archive terminal proposal — left in place (not re-executed)"
            ),
        }
    }

    /// Apply the grant a human approved on a **permission request** (`proposal.requested_grant` set),
    /// per the scope they chose (`proposal.approved_scope`). The blocked call itself already executed
    /// in `handle_proposal_change` (the human tap was the gate); this decides only whether *future*
    /// calls of the same shape still have to ask:
    ///
    /// - `Everywhere` → persist to the machine-owned overlay (durable; takes effect at the next boot
    ///   / container recreate, when config is re-loaded). Only a human button tap ever reaches here,
    ///   so the "agents can't edit their own permission config" invariant holds.
    /// - `Session` → process-lifetime, in-memory grant via `liberado_common::session_grants`, keyed by
    ///   the proposal's pool. Folded post-narrow into that pool's effective ceiling by
    ///   `Orchestrator::run`, so the next same-zone write in this process passes without a prompt.
    ///   Lost on restart (the in-memory counterpart to Everywhere's on-disk overlay).
    /// - `Once` / `None` → nothing to persist.
    ///
    /// Best-effort: a persistence failure is logged, never propagated — the approved call already ran.
    pub(crate) fn apply_approved_grant(&self, proposal: &liberado_common::Proposal) {
        let Some(capability) = &proposal.requested_grant else {
            return; // ordinary proposal, not a permission request
        };
        let component = grant_component_for_pool(proposal.pool.as_deref());
        match proposal.approved_scope {
            Some(liberado_common::GrantScope::Everywhere) => {
                match liberado_config::append_grant_to_overlay(component, capability) {
                    Ok(true) => tracing::info!(
                        component,
                        ?capability,
                        "persisted 'everywhere' grant to the machine-owned overlay \
                         (effective on next boot)"
                    ),
                    Ok(false) => tracing::info!(
                        component,
                        ?capability,
                        "'everywhere' grant already present in the overlay — no change"
                    ),
                    Err(e) => tracing::error!(
                        component,
                        ?capability,
                        error = %e,
                        "failed to persist 'everywhere' grant to the overlay \
                         (the approved call still ran)"
                    ),
                }
            }
            Some(liberado_common::GrantScope::Session) => {
                // Process-lifetime, in-memory grant (gone on restart) — the counterpart to
                // Everywhere's on-disk overlay. Keyed by the proposal's POOL (not the config
                // component), because that's what the live orchestrator reads back via
                // `session_grants::session_grant(&self.pool_name)`. Folded post-narrow into the pool's
                // effective ceiling, so the next same-zone write in this process passes with no prompt.
                let pool = proposal.pool.as_deref().unwrap_or(DEFAULT_POOL);
                let newly =
                    liberado_common::session_grants::grant_for_session(pool, capability.clone());
                tracing::info!(
                    pool,
                    ?capability,
                    newly,
                    "applied 'session' grant (process-lifetime; in memory, lost on restart)"
                );
            }
            Some(liberado_common::GrantScope::Once) | None => {}
        }
    }
}
