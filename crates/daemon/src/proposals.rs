//! Proposal write, approval handling, archive, and grant application.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use liberado_common::{
    DEFAULT_POOL, PROPOSALS_DIR, Proposal, ProposalStatus, SignedProposal, WriteProvenance,
};
use liberado_orchestrator::{Disposition, EXPIRED_PROPOSAL_REFUSAL_SUMMARY};
use liberado_vault::{Vault, VaultError};
use tokio::fs;

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
    /// proposal, execute its action and flip it to `done`. Wall-clock-expired notes (regardless of
    /// frontmatter status) are never executed: they are flipped to `status: expired` and archived
    /// so the active dir stays tidy without waiting for the background reaper. Terminal
    /// statuses / unparseable notes are observed (and terminal notes archived if still active).
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

        // 4. Wall-clock past `expires` — never execute, even if frontmatter still says pending or
        //    approved. Complete the expiry lifecycle here (status + archive) so a human touch after
        //    the deadline cleans the active dir without waiting for the background reaper. Same end
        //    state as `reap_expired_proposals`; either path may own the cleanup.
        if proposal.is_expired_at(chrono::Utc::now()) {
            tracing::info!(
                proposal_id = %proposal.id,
                prior_status = ?proposal.status,
                "proposal is past expires — marking expired and archiving (not executing)"
            );
            let provenance =
                liberado_common::WriteProvenance::agent(DAEMON_SOURCE, &proposal.correlation_id);
            proposal.status = liberado_common::ProposalStatus::Expired;
            if let Err(e) = self
                .vault
                .write(rel_path, &proposal.to_note(), None, &provenance)
                .await
            {
                // Leave in place for the reaper / a later touch; never execute a past-deadline note.
                tracing::warn!(
                    error = %e,
                    proposal_id = %proposal.id,
                    path = %rel_path.display(),
                    "failed to mark proposal expired on reactive path — left in place"
                );
                return Ok(ReactionOutcome::Observed);
            }
            self.archive_terminal_proposal(rel_path, &proposal).await;
            return Ok(ReactionOutcome::Observed);
        }

        // 5. Only Approved is actionable — the note claims something other than approval.
        if !proposal.status.is_actionable() {
            tracing::debug!(status = ?proposal.status, "proposal is not actionable");
            return Ok(ReactionOutcome::Observed);
        }

        // 5.5. ...and the note's claim is not sufficient. `status` lives in `proposals/`, which
        //      policy declares `agent_writable`, and it is deliberately outside the integrity
        //      signature so a human can flip it without invalidating the hash. Both are right
        //      alone; together they left the approval field of the approval mechanism writable by
        //      the thing being gated. Provenance cannot rescue it either — MCP tool writes carry
        //      none into the audit log, so an agent's `edit_note` attributes as `External`, which
        //      this system reads as *human*.
        //
        //      So the authority is the ledger under `<LIBERADO_DATA_DIR>/`, which no MCP mounts and
        //      no tool addresses. The note is a view. Absent, unreadable, or corrupt, it authorises
        //      nothing — a proposal needing re-approval is a far better failure than one that runs
        //      because a file could not be read.
        let Some(ledger) = &self.approvals else {
            tracing::warn!(
                proposal_id = %proposal.id,
                "no approval ledger attached; refusing to execute (build the daemon with                  `with_approval_ledger`)"
            );
            return Ok(ReactionOutcome::Observed);
        };
        match ledger.decision_for(&proposal.id).await {
            Some(liberado_common::ApprovalDecision::Approved) => {}
            other => {
                tracing::warn!(
                    proposal_id = %proposal.id,
                    ledger_decision = ?other,
                    path = %rel_path.display(),
                    "proposal note says approved but no human approval is recorded — refusing.                      The note is a view; approval is recorded out of band."
                );
                return Ok(ReactionOutcome::Observed);
            }
        }

        // 6. Execute — via the *same* pool this proposal was proposed under (Decision 18
        //    checkpoint #3), never a different one, so a restricted pool's proposal can never
        //    execute with a different (possibly broader) pool's authority. `Orchestrator::
        //    execute_approved` itself defensively re-checks this too (defense in depth).
        //    An orchestration error is an infra failure and propagates (so it can be retried on
        //    the next watch cycle). We do NOT mark done on failure.
        //
        // Re-check wall-clock expiry immediately before execute: step 4 may have passed while we
        // awaited other work, and the reaper deliberately skips Approved notes that may be mid-flight.
        if proposal.is_expired_at(chrono::Utc::now()) {
            tracing::info!(
                proposal_id = %proposal.id,
                "approved proposal expired before execute — marking expired and archiving"
            );
            let provenance =
                liberado_common::WriteProvenance::agent(DAEMON_SOURCE, &proposal.correlation_id);
            proposal.status = liberado_common::ProposalStatus::Expired;
            if let Err(e) = self
                .vault
                .write(rel_path, &proposal.to_note(), None, &provenance)
                .await
            {
                tracing::warn!(
                    error = %e,
                    proposal_id = %proposal.id,
                    "failed to mark late-expired proposal — left in place"
                );
                return Ok(ReactionOutcome::Observed);
            }
            self.archive_terminal_proposal(rel_path, &proposal).await;
            return Ok(ReactionOutcome::Observed);
        }

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
        // The approval path is the other way inference escapes attribution. The proposal already
        // carries the originating `correlation_id` — it is used for write provenance a few frames
        // down — but nothing put it on the latency task-local, so every model call an approved
        // subagent makes recorded `"-"`. On the deployed journal that was 14 of 104 unattributed
        // calls, the expensive kind: agent loops reaching 29k prompt tokens.
        let report = liberado_provider::latency::with_correlation(
            proposal.correlation_id.clone(),
            orch.execute_approved(&proposal),
        )
        .await?;

        // 6.5. Orchestrator pre-execution refuse (wall-clock expiry race): tools never ran —
        //     complete the expiry lifecycle without applying permission grants or marking Done.
        //     Match the refusal summary **exactly** (not substring) so free-form Failed reports
        //     that mention "expired" are not misclassified.
        if report.outcome == liberado_common::Outcome::Failed
            && report.summary == EXPIRED_PROPOSAL_REFUSAL_SUMMARY
        {
            tracing::info!(
                proposal_id = %proposal.id,
                "execute_approved refused expired proposal — completing expiry lifecycle"
            );
            let provenance =
                liberado_common::WriteProvenance::agent(DAEMON_SOURCE, &proposal.correlation_id);
            proposal.status = liberado_common::ProposalStatus::Expired;
            let _ = self
                .vault
                .write(rel_path, &proposal.to_note(), None, &provenance)
                .await;
            self.archive_terminal_proposal(rel_path, &proposal).await;
            return Ok(ReactionOutcome::Observed);
        }

        // 6.6. Permission grant only after tools were allowed to run (human tap was the gate).
        //     Pre-execution refuses (expiry above; integrity/pool refuse still returns Failed with
        //     a different summary and must not grant either — those still reach Done historically
        //     only when mis-routed; we do not grant when outcome is a pure preflight refuse).
        //     Best-effort: a persistence failure never fails the reaction.
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
    /// `proposals/archive/<outcome>/`. See [`archive_terminal_proposal_note`] for the shared
    /// semantics used by both the reactive approve path and the background expiry reaper.
    pub(crate) async fn archive_terminal_proposal(
        &self,
        rel_path: &Path,
        proposal: &liberado_common::Proposal,
    ) {
        archive_terminal_proposal_note(&self.vault, rel_path, proposal).await;
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
///
/// Shared by [`Daemon::archive_terminal_proposal`] (reactive path) and
/// [`reap_expired_proposals`] (background reaper). The reaper cannot go through the watch
/// pipeline: its Expired write uses agent provenance and is attribution-suppressed, so archive
/// must happen inline here.
pub(crate) async fn archive_terminal_proposal_note(
    vault: &Vault,
    rel_path: &Path,
    proposal: &Proposal,
) {
    let Some(outcome) = archive_outcome_subdir(proposal.status) else {
        return; // not terminal — nothing to archive
    };
    let Some(file_name) = rel_path.file_name().and_then(|n| n.to_str()) else {
        tracing::warn!(path = %rel_path.display(), "proposal path has no file name — not archiving");
        return;
    };
    let dest = format!("{PROPOSALS_ARCHIVE_DIR}/{outcome}/{file_name}");
    let provenance = WriteProvenance::agent(DAEMON_SOURCE, &proposal.correlation_id);
    match vault.move_note(rel_path, &dest, None, &provenance).await {
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

/// Background loop: every `interval`, scan `proposals/` for `.md` files whose `expires` date has
/// passed and flip `status: expired` + archive. A zero `interval` is a no-op (disabled).
pub(crate) async fn proposal_reap_loop(vault: Vault, interval: Duration) {
    if interval.is_zero() {
        return;
    }
    let mut tick = tokio::time::interval(interval);
    // Skip immediate fire — let the daemon run for one interval before the first periodic sweep.
    tick.tick().await;
    loop {
        tick.tick().await;
        if let Err(e) = reap_expired_proposals(&vault).await {
            tracing::warn!(error = %e, "proposal reaper sweep failed");
        }
    }
}

/// How long past `expires` an **Approved** proposal must sit before the reaper will claim it.
///
/// Approved notes are the reactive path's to finish: one may be mid-`execute_approved` right as the
/// deadline passes, and reaping it would race the Done write. But the reactive path only ever runs
/// on a human edit, so an Approved note that expires and is never touched again would otherwise sit
/// in the active dir forever. Waiting this long makes both true: far longer than any execute, far
/// shorter than "forever".
const APPROVED_REAP_GRACE: chrono::Duration = chrono::Duration::hours(1);

/// Sweep `proposals/` once: read every `.md` file (excluding the archive subtree), check
/// `is_expired_at(Utc::now())`, and if so write `status: expired` + archive into
/// `proposals/archive/expired/`.
///
/// Ownership between this and the reactive path (`handle_proposal_change`) is by status:
/// **Pending** is reaper-owned immediately; **Approved** only after [`APPROVED_REAP_GRACE`] past
/// the deadline, so an in-flight execute finishes first; terminal statuses are nobody's.
///
/// Per-file failures (read, write, archive) are logged and skipped so one bad note cannot starve
/// the rest of the directory. Only structural failures (cannot list `proposals/`) abort the sweep.
pub(crate) async fn reap_expired_proposals(vault: &Vault) -> Result<(), DaemonError> {
    let proposals_path = vault.root().join(PROPOSALS_DIR);

    let mut reader = match fs::read_dir(&proposals_path).await {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(DaemonError::from(VaultError::Backend(e.to_string()))),
    };

    // Materialize the directory listing before mutating (archive moves files out of the active
    // dir). Mutating while iterating `read_dir` is platform-dependent.
    let mut entries = Vec::new();
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|e| DaemonError::from(VaultError::Backend(e.to_string())))?
    {
        entries.push(entry);
    }

    let now = Utc::now();

    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }

        // Convert the absolute filesystem path to a vault-relative one.
        let rel_path = match vault.to_relative(&path) {
            Some(p) => p,
            None => continue,
        };

        // Skip the archive subtree — archived entries are already terminal.
        if rel_path.starts_with(PROPOSALS_ARCHIVE_DIR) {
            continue;
        }

        let content = match vault.read(&rel_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %rel_path.display(),
                    "proposal reaper: read failed — skipping"
                );
                continue;
            }
        };

        let mut proposal = match Proposal::from_note(&content) {
            Ok(p) => p,
            Err(_) => continue, // not a parseable proposal
        };

        if !proposal.is_expired_at(now) {
            continue;
        }
        if proposal.status.is_terminal() {
            continue; // already handled by handle_proposal_change
        }
        match proposal.status {
            // Reaper-owned as soon as the deadline passes — nothing is executing a Pending note.
            ProposalStatus::Pending => {}
            // Approved may be mid-execute on the reactive path right at the deadline, so leave it
            // alone until well past it. Without this arm an Approved note nobody touches again
            // never leaves the active dir, since the reactive path only runs on a human edit.
            ProposalStatus::Approved => {
                let past_deadline = proposal
                    .expires
                    .is_some_and(|expires| now - expires >= APPROVED_REAP_GRACE);
                if !past_deadline {
                    continue;
                }
                tracing::info!(
                    proposal_id = %proposal.id,
                    "approved proposal expired over the grace window ago and was never \
                     resolved — reaping"
                );
            }
            // Anything else is not the reaper's to move.
            _ => continue,
        }

        let provenance = WriteProvenance::agent(DAEMON_SOURCE, &proposal.correlation_id);
        proposal.status = ProposalStatus::Expired;
        if let Err(e) = vault
            .write(&rel_path, &proposal.to_note(), None, &provenance)
            .await
        {
            // One unwritable note must not abort the sweep for every later entry.
            tracing::warn!(
                error = %e,
                proposal_id = %proposal.id,
                path = %rel_path.display(),
                "proposal reaper: failed to mark expired — continuing sweep"
            );
            continue;
        }

        tracing::info!(
            proposal_id = %proposal.id,
            "marked proposal expired (reaper)"
        );

        // Archive inline: the Expired write above is DAEMON_SOURCE and will not re-enter
        // `handle_proposal_change`, so the reactive terminal-archive branch never sees it.
        archive_terminal_proposal_note(vault, &rel_path, &proposal).await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use liberado_common::{Proposal, ProposalStatus, ProposedAction, ToolCall, WriteProvenance};
    use liberado_vault::Vault;
    use std::fs;
    use tempfile::TempDir;

    fn expired_proposal() -> Proposal {
        let mut p = Proposal::pending(
            "test-reap",
            "corr-reap-1",
            "test-agent",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "noop".to_string(),
                args: serde_json::json!({}),
            }]),
            "Expired test proposal",
        );
        p.expires = Some(Utc::now() - ChronoDuration::hours(1));
        p.created = Utc::now() - ChronoDuration::hours(2);
        p
    }

    fn expired_proposal_named(id: &str, corr: &str) -> Proposal {
        let mut p = Proposal::pending(
            id,
            corr,
            "test-agent",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "noop".to_string(),
                args: serde_json::json!({}),
            }]),
            "Expired test proposal",
        );
        p.expires = Some(Utc::now() - ChronoDuration::hours(1));
        p.created = Utc::now() - ChronoDuration::hours(2);
        p
    }

    fn live_proposal() -> Proposal {
        let mut p = Proposal::pending(
            "test-live",
            "corr-live-1",
            "test-agent",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "noop".to_string(),
                args: serde_json::json!({}),
            }]),
            "Still-valid test proposal",
        );
        p.expires = Some(Utc::now() + ChronoDuration::hours(1));
        p
    }

    /// Restore write permission so TempDir cleanup succeeds after the read-only-file test.
    ///
    /// Gated with its only caller: that test is Windows-only, so on Unix this would be dead code
    /// and `-D warnings` would fail the build.
    #[cfg(windows)]
    fn clear_readonly(path: &std::path::Path) {
        let Ok(meta) = fs::metadata(path) else {
            return;
        };
        let mut perms = meta.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o644);
        }
        #[cfg(not(unix))]
        {
            // Windows only: clear the read-only attribute. Clippy's unix-oriented lint still fires.
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
        }
        let _ = fs::set_permissions(path, perms);
    }

    #[tokio::test]
    async fn reap_flips_expired_pending_to_expired_and_archives() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        let prov = WriteProvenance::agent("test", "c1");

        let proposals_dir = dir.path().join(PROPOSALS_DIR);
        tokio::fs::create_dir_all(&proposals_dir).await.unwrap();
        vault
            .write(
                "proposals/old-one.md",
                &expired_proposal().to_note(),
                None,
                &prov,
            )
            .await
            .unwrap();
        vault
            .write(
                "proposals/still-valid.md",
                &live_proposal().to_note(),
                None,
                &prov,
            )
            .await
            .unwrap();

        reap_expired_proposals(&vault).await.unwrap();

        // Expired note left the active dir and lives under archive/expired/ with status Expired.
        assert!(
            vault.read("proposals/old-one.md").await.is_err(),
            "expired proposal must leave the active proposals/ dir"
        );
        let archived = vault
            .read("proposals/archive/expired/old-one.md")
            .await
            .expect("expired proposal must be archived under archive/expired/");
        let parsed = Proposal::from_note(&archived).unwrap();
        assert_eq!(parsed.status, ProposalStatus::Expired);

        // Still-valid pending note is untouched.
        let live = vault.read("proposals/still-valid.md").await.unwrap();
        let parsed = Proposal::from_note(&live).unwrap();
        assert_eq!(parsed.status, ProposalStatus::Pending);
    }

    /// Windows only, because of how the failure is injected.
    ///
    /// turbovault writes temp-then-rename. Windows refuses to rename over a read-only
    /// destination, so marking the file read-only does fail the write. On Unix, rename permission
    /// comes from the *directory*, not the target file — the read-only bit is inert there, the
    /// rename succeeds, and the note is archived instead of being left behind. This test asserted
    /// a Windows filesystem behaviour and silently passed here while being wrong on Linux; ubuntu
    /// had never reached the test step to say so. See the Unix counterpart below.
    #[cfg(windows)]
    #[tokio::test]
    async fn reap_continues_sweep_when_one_write_fails() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        let prov = WriteProvenance::agent("test", "c1");

        let proposals_dir = dir.path().join(PROPOSALS_DIR);
        tokio::fs::create_dir_all(&proposals_dir).await.unwrap();

        // Two expired notes. Make one unwritable so the Expired rewrite fails for that file only.
        vault
            .write(
                "proposals/stuck.md",
                &expired_proposal_named("stuck", "corr-stuck").to_note(),
                None,
                &prov,
            )
            .await
            .unwrap();
        vault
            .write(
                "proposals/ok.md",
                &expired_proposal_named("ok", "corr-ok").to_note(),
                None,
                &prov,
            )
            .await
            .unwrap();

        let stuck_abs = proposals_dir.join("stuck.md");
        let mut perms = fs::metadata(&stuck_abs).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&stuck_abs, perms).unwrap();

        // Sweep must succeed overall even though stuck.md cannot be rewritten.
        reap_expired_proposals(&vault)
            .await
            .expect("sweep must not abort on per-file write failure");

        // Writable expired note is expired + archived.
        assert!(
            vault.read("proposals/ok.md").await.is_err(),
            "writable expired proposal must leave active dir"
        );
        let archived = vault
            .read("proposals/archive/expired/ok.md")
            .await
            .expect("writable expired proposal must be archived");
        assert_eq!(
            Proposal::from_note(&archived).unwrap().status,
            ProposalStatus::Expired
        );

        // Unwritable note remains in place (still pending — write never landed).
        let stuck = vault
            .read("proposals/stuck.md")
            .await
            .expect("unwritable note stays in active dir");
        assert_eq!(
            Proposal::from_note(&stuck).unwrap().status,
            ProposalStatus::Pending,
            "failed write must not partially mutate status"
        );

        clear_readonly(&stuck_abs);
    }

    /// Unix counterpart to the test above.
    ///
    /// Without root there is no way to make one file un-replaceable while its siblings stay
    /// writable — atomic rename only consults the directory — so this cannot also show a sibling
    /// note being archived in the same sweep. It pins the other half of the invariant: a failed
    /// rewrite is survivable (the sweep returns Ok rather than aborting) and does not partially
    /// mutate the note, which stays Pending rather than becoming Expired-in-place.
    #[cfg(unix)]
    #[tokio::test]
    async fn reap_survives_a_failing_write() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        let prov = WriteProvenance::agent("test", "c1");

        let proposals_dir = dir.path().join(PROPOSALS_DIR);
        tokio::fs::create_dir_all(&proposals_dir).await.unwrap();
        vault
            .write(
                "proposals/stuck.md",
                &expired_proposal_named("stuck", "corr-stuck").to_note(),
                None,
                &prov,
            )
            .await
            .unwrap();

        // r-x: the reaper can still list the directory and read the note, but cannot create the
        // temp file its write needs.
        fs::set_permissions(&proposals_dir, fs::Permissions::from_mode(0o555)).unwrap();

        let swept = reap_expired_proposals(&vault).await;

        // Restore before asserting — a read-only directory would otherwise defeat TempDir cleanup
        // even if an assertion below fails.
        fs::set_permissions(&proposals_dir, fs::Permissions::from_mode(0o755)).unwrap();

        swept.expect("sweep must not abort on per-file write failure");

        let stuck = vault
            .read("proposals/stuck.md")
            .await
            .expect("unwritable note stays in active dir");
        assert_eq!(
            Proposal::from_note(&stuck).unwrap().status,
            ProposalStatus::Pending,
            "failed write must not partially mutate status"
        );
    }

    #[tokio::test]
    async fn reap_skips_non_md_files_and_archive_subtree() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        let prov = WriteProvenance::agent("test", "c1");

        // Bare proposals/ dir might not exist → reaper must not crash.
        reap_expired_proposals(&vault).await.unwrap();

        let proposals_dir = dir.path().join(PROPOSALS_DIR);
        tokio::fs::create_dir_all(&proposals_dir).await.unwrap();
        tokio::fs::write(proposals_dir.join("notes.txt"), "Not a proposal")
            .await
            .unwrap();

        let archive_dir = proposals_dir.join("archive").join("expired");
        tokio::fs::create_dir_all(&archive_dir).await.unwrap();
        vault
            .write(
                "proposals/archive/expired/already-archived.md",
                &expired_proposal().to_note(),
                None,
                &prov,
            )
            .await
            .unwrap();

        reap_expired_proposals(&vault).await.unwrap();

        let content = vault
            .read("proposals/archive/expired/already-archived.md")
            .await
            .unwrap();
        let parsed = Proposal::from_note(&content).unwrap();
        assert_ne!(
            parsed.status,
            ProposalStatus::Expired,
            "archived proposals must be skipped"
        );
    }

    #[tokio::test]
    async fn reap_skips_already_terminal_proposals() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        let prov = WriteProvenance::agent("test", "c1");

        let proposals_dir = dir.path().join(PROPOSALS_DIR);
        tokio::fs::create_dir_all(&proposals_dir).await.unwrap();
        let mut p = expired_proposal();
        p.status = ProposalStatus::Rejected;
        vault
            .write("proposals/rejected.md", &p.to_note(), None, &prov)
            .await
            .unwrap();

        reap_expired_proposals(&vault).await.unwrap();

        let content = vault.read("proposals/rejected.md").await.unwrap();
        let parsed = Proposal::from_note(&content).unwrap();
        assert_eq!(parsed.status, ProposalStatus::Rejected);
    }

    #[tokio::test]
    async fn reap_skips_recently_expired_approved_proposals() {
        // Approved notes near the deadline may be mid-execute on the reactive path; the reaper
        // must not race the Done write by archiving them out from under it.
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        let prov = WriteProvenance::agent("test", "c1");
        let proposals_dir = dir.path().join(PROPOSALS_DIR);
        tokio::fs::create_dir_all(&proposals_dir).await.unwrap();

        let mut p = expired_proposal_named("approved-late", "corr-approved");
        p.status = ProposalStatus::Approved;
        // Expired, but only just — inside the grace window.
        p.expires = Some(Utc::now() - ChronoDuration::minutes(1));
        vault
            .write("proposals/approved-late.md", &p.to_note(), None, &prov)
            .await
            .unwrap();

        reap_expired_proposals(&vault).await.unwrap();

        let content = vault
            .read("proposals/approved-late.md")
            .await
            .expect("approved expired note must stay in active dir inside the grace window");
        assert_eq!(
            Proposal::from_note(&content).unwrap().status,
            ProposalStatus::Approved
        );
        assert!(
            vault
                .read("proposals/archive/expired/approved-late.md")
                .await
                .is_err(),
            "reaper must not archive a just-expired Approved note"
        );
    }

    #[tokio::test]
    async fn reap_claims_approved_proposals_left_past_the_grace_window() {
        // The reactive path only runs on a human edit, so an Approved note that expires and is
        // never touched again would sit in the active dir forever. Past the grace window no
        // execute can still be in flight, so the reaper completes the lifecycle.
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        let prov = WriteProvenance::agent("test", "c1");
        let proposals_dir = dir.path().join(PROPOSALS_DIR);
        tokio::fs::create_dir_all(&proposals_dir).await.unwrap();

        let mut p = expired_proposal_named("approved-stranded", "corr-stranded");
        p.status = ProposalStatus::Approved;
        p.expires = Some(Utc::now() - APPROVED_REAP_GRACE - ChronoDuration::minutes(1));
        vault
            .write("proposals/approved-stranded.md", &p.to_note(), None, &prov)
            .await
            .unwrap();

        reap_expired_proposals(&vault).await.unwrap();

        assert!(
            vault.read("proposals/approved-stranded.md").await.is_err(),
            "a long-stranded approved note must leave the active dir"
        );
        let archived = vault
            .read("proposals/archive/expired/approved-stranded.md")
            .await
            .expect("stranded approved note must be archived under archive/expired/");
        assert_eq!(
            Proposal::from_note(&archived).unwrap().status,
            ProposalStatus::Expired,
            "it expired without executing — never Done"
        );
    }
}
