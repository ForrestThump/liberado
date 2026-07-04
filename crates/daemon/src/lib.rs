//! # liberado-daemon
//!
//! The long-running core of Liberado (Decision 2: daemon-first). This is the v1 **vertical
//! slice**: it watches the vault, attributes every observed change (loop-breaking, Decision 5),
//! and forwards the changes that came from *outside* our own write path as standardized
//! [`Event`]s. Downstream (the dispatcher, hooks) consume those events; here we just produce them.
//!
//! The reactive decision is split into a pure, deterministic [`Daemon::process_change`] (testable
//! without the filesystem) and the watcher plumbing in [`Daemon::run`] — mirroring how the vault
//! crate separates attribution from I/O.

mod debounce;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use debounce::Debouncer;
use liberado_common::{
    CapabilitySet, CapabilityCatalog, DispatchDecision, Event, EventPayload, ProposalSigner,
    WriteClass, event_source,
};
use liberado_dispatcher::{DispatchRequest, Dispatcher};
use liberado_notify::Notifier;
use liberado_orchestrator::{Disposition, Orchestrator, OrchestratorError};
use liberado_vault::{Attribution, Vault, VaultError, VaultEvent};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;

/// Default debounce window: long enough to coalesce a `notify` burst, short enough to feel live.
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Depth assigned to a daemon-originated reaction. It is the first agent step reacting to an
/// external change, so it starts the correlation chain at 1 (the depth cap halts longer cascades).
const DEFAULT_REACTION_DEPTH: u32 = 1;

/// Prefix of the correlation id minted for a vault-change reaction, and how many hex chars of the
/// content hash to append (enough to distinguish edits; the full id stays short for logs/journals).
const CORRELATION_PREFIX: &str = "vault-change";
const CORRELATION_HASH_LEN: usize = 12;

/// Provenance source recorded for the daemon's own vault writes (e.g. a proposal artifact). Agent
/// provenance is what makes attribution suppress the write, so the daemon won't react to the
/// proposal it just wrote (loop-break, Decision 5).
const DAEMON_SOURCE: &str = "liberado";

/// Directory under which proposal artifacts live. Used to route external edits on proposal notes
/// through the approve→execute path instead of the dispatcher.
const PROPOSALS_DIR: &str = "proposals";

/// What the daemon produces for each external change it reacts to: the standardized event plus how
/// far it took the reaction.
pub struct Reaction {
    pub event: Event,
    pub outcome: ReactionOutcome,
}

/// How far a reaction progressed, depending on what's attached to the daemon.
pub enum ReactionOutcome {
    /// Watch-only — no dispatcher attached. The event is surfaced but not routed.
    Observed,
    /// A dispatcher routed it to a decision, but no orchestrator is attached to execute it.
    Decided(DispatchDecision),
    /// A dispatcher decided and an orchestrator executed it (or surfaced a `Clarify`).
    Acted(Disposition),
}

impl ReactionOutcome {
    /// A short label for tracing.
    pub fn label(&self) -> &'static str {
        match self {
            ReactionOutcome::Observed => "(observed)",
            ReactionOutcome::Decided(d) => d.action.label(),
            ReactionOutcome::Acted(Disposition::Reported(_)) => "acted:reported",
            ReactionOutcome::Acted(Disposition::Clarify { .. }) => "acted:clarify",
            ReactionOutcome::Acted(Disposition::Propose(_)) => "acted:proposed",
        }
    }
}

/// The dispatcher plus the disjoint context the daemon hands it for each reaction.
struct DispatcherContext {
    dispatcher: Dispatcher,
    /// Shared with the server's `/api/catalog` and (when attached) chat's own dispatch — one
    /// live source snapshotted fresh per request, not a copy frozen at construction time.
    catalog: Arc<CapabilityCatalog>,
    capabilities: CapabilitySet,
    reaction_depth: u32,
    /// `(zone, write_class)` pairs from `Policy.zones` — what the zone-write-class guard (§6 #2)
    /// checks a seed call's resolved target zone against.
    zone_write_classes: Vec<(String, WriteClass)>,
}

impl DispatcherContext {
    /// Turn a vault-change event into a self-contained dispatch request.
    fn dispatch_request(&self, event: &Event) -> DispatchRequest {
        let path = event.payload.path.as_deref().unwrap_or("(unknown)");
        DispatchRequest {
            goal: format!(
                "A note in the vault was created or edited at '{path}'. Decide how to react to it."
            ),
            catalog: self.catalog.descriptors(),
            capabilities: self.capabilities.clone(),
            reaction_depth: self.reaction_depth,
            zone_write_classes: self.zone_write_classes.clone(),
        }
    }
}

/// The event type emitted for an attributed-external vault change.
pub const VAULT_NOTE_CHANGED: &str = "VaultNoteChanged";

/// Errors from the daemon.
#[derive(Debug, Error)]
pub enum DaemonError {
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("orchestration failed: {0}")]
    Orchestrator(#[from] OrchestratorError),
}

/// The Liberado daemon.
pub struct Daemon {
    vault: Vault,
    debounce: Duration,
    dispatcher: Option<DispatcherContext>,
    orchestrator: Option<Orchestrator>,
    /// Verifies a proposal's integrity signature before treating an approval edit as actionable
    /// (see `handle_proposal_change`). Defaults to a fresh random key at `open()` — production
    /// wiring overrides it via [`with_proposal_signer`](Self::with_proposal_signer) with the same
    /// installation-wide signer every proposal-creation site uses, so signatures actually match.
    signer: ProposalSigner,
    /// Told about every proposal this daemon writes (dispatcher pre-flight `Propose` path) —
    /// optional, `None` by default. Best-effort: a notification failure never blocks the write.
    notifier: Option<Arc<dyn Notifier>>,
}

impl Daemon {
    /// Open the daemon over the vault at `vault_path` (enables the audit log).
    pub async fn open(
        name: impl Into<String>,
        vault_path: impl Into<PathBuf>,
    ) -> Result<Self, DaemonError> {
        Ok(Self {
            vault: Vault::open(name, vault_path).await?,
            debounce: DEFAULT_DEBOUNCE,
            dispatcher: None,
            orchestrator: None,
            signer: ProposalSigner::random(),
            notifier: None,
        })
    }

    /// Override the debounce window (e.g. a short window in tests).
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// Attach a [`Notifier`] to tell about every proposal this daemon writes directly (the
    /// dispatcher pre-flight `Propose` path — see `write_proposal`). Optional; a daemon with none
    /// attached just never sends anything, the same as today.
    pub fn with_notifier(mut self, notifier: Arc<dyn Notifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    /// Use `signer` to verify proposal integrity signatures, instead of the random ephemeral one
    /// `open()` generates by default. Production callers pass the same installation-wide signer
    /// every proposal-creation site (`Orchestrator`, `RiskGatedToolRuntime`) uses — see
    /// `liberado_bootstrap::configure_daemon`.
    pub fn with_proposal_signer(mut self, signer: ProposalSigner) -> Self {
        self.signer = signer;
        self
    }

    /// Attach a dispatcher so reactable changes are routed to a [`DispatchDecision`]. Without one,
    /// the daemon runs in watch-only mode ([`ReactionOutcome::Observed`]). The `catalog` +
    /// `capabilities` form the disjoint context the dispatcher reasons over. `catalog` is the same
    /// shared, live `CapabilityCatalog` the server's API and (when attached) chat's own dispatch
    /// read — the daemon snapshots it fresh per reaction, not once at construction.
    pub fn with_dispatcher(
        mut self,
        dispatcher: Dispatcher,
        catalog: Arc<CapabilityCatalog>,
        capabilities: CapabilitySet,
    ) -> Self {
        self.dispatcher = Some(DispatcherContext {
            dispatcher,
            catalog,
            capabilities,
            reaction_depth: DEFAULT_REACTION_DEPTH,
            zone_write_classes: Vec::new(),
        });
        self
    }

    /// Override the zone-write-classes the zone-write-class guard (§6 #2) checks against —
    /// `(zone, write_class)` pairs from `Policy.zones`. Only meaningful alongside
    /// [`with_dispatcher`](Self::with_dispatcher); a no-op if called before it (there's no
    /// `DispatcherContext` yet to attach to).
    pub fn with_zone_write_classes(mut self, zone_write_classes: Vec<(String, WriteClass)>) -> Self {
        if let Some(ctx) = &mut self.dispatcher {
            ctx.zone_write_classes = zone_write_classes;
        }
        self
    }

    /// Attach an orchestrator so decisions are **executed** (the reaction yields
    /// [`ReactionOutcome::Acted`]). Only meaningful alongside [`with_dispatcher`](Self::with_dispatcher):
    /// without a dispatcher there is no decision to execute; with a dispatcher but no orchestrator,
    /// reactions stop at [`ReactionOutcome::Decided`].
    ///
    /// This crate has no `liberado-mcp` dependency (by design — see `ARCHITECTURE.md`), so the
    /// requirement that `orchestrator` carry a real `RuntimeFactory` implementation for production
    /// use is invisible from this crate's own `Cargo.toml` alone. That wiring is supplied by the
    /// caller — see `liberado_bootstrap::configure_daemon`, which connects an `McpRegistry` and
    /// passes it in.
    pub fn with_orchestrator(mut self, orchestrator: Orchestrator) -> Self {
        self.orchestrator = Some(orchestrator);
        self
    }

    /// The underlying vault handle (cheap to clone).
    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    /// The pure reactive decision: given an observed change to `rel_path`, return a reactable
    /// [`Event`], or `None` if the change was one of our own writes (suppressed by the hash-join)
    /// or the path is gone. No filesystem watching here — this is the unit-testable core.
    pub async fn process_change(&self, rel_path: &Path) -> Result<Option<Event>, DaemonError> {
        match self.vault.attribute(rel_path).await? {
            Attribution::External => Ok(Some(self.build_event(rel_path).await)),
            // Our own write (don't react to ourselves) or a vanished path.
            Attribution::Agent(_) | Attribution::Missing => Ok(None),
        }
    }

    /// Build the standardized event for an attributed-external change. The `correlation_id` keys
    /// idempotency/loop-breaking downstream; it is derived from the path + a short content hash so
    /// distinct edits are distinct events while a redelivery of the same state is not.
    async fn build_event(&self, rel_path: &Path) -> Event {
        let content = self.vault.read(rel_path).await.unwrap_or_default();
        let hash = Vault::content_hash(&content);
        let rel = rel_path.to_string_lossy().replace('\\', "/");
        // `get(..N)` (not `[..N]`) is panic-safe regardless of the hash's byte boundaries.
        let short_hash = hash.get(..CORRELATION_HASH_LEN).unwrap_or(&hash);
        let correlation_id = format!("{CORRELATION_PREFIX}:{rel}:{short_hash}");
        Event::trigger(
            VAULT_NOTE_CHANGED,
            event_source::TURBOVAULT_SUBSCRIPTION,
            correlation_id,
            EventPayload {
                path: Some(rel),
                ..Default::default()
            },
        )
    }

    /// Take a reactable change as far as the attached components allow: observe → decide → act.
    /// Failures at any stage are logged and degrade the outcome (never abort the watch loop).
    ///
    /// Edits under `proposals/` bypass the dispatcher — they are evaluated directly as potential
    /// proposal approvals (the human's Obsidian edit is the authorization).
    async fn react(&self, event: &Event) -> ReactionOutcome {
        // Before any dispatch: check if this is a proposal note change. The human's edit (status
        // approval) is the authorization — no need to re-dispatch (which would re-propose).
        if let Some(path) = event.payload.path.as_deref() {
            // The path was normalized to forward slashes in build_event, so starts_with works on
            // both platforms. Exclude the exact `proposals` directory path to avoid attempting to
            // read a directory as a proposal note on directory-creation watch events.
            if path.starts_with(PROPOSALS_DIR) && path != Path::new(PROPOSALS_DIR) {
                // Infra errors (orchestrator runtime_for failure) are logged but degraded to
                // Observed so the watch loop never crashes. The proposal is NOT marked done when an
                // error occurs, so a human re-triggering the file (or a future retry mechanism) can
                // pick it up again.
                return self
                    .handle_proposal_change(Path::new(path))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!(error = %e, "proposal change handling failed — not marked done, retriable");
                        ReactionOutcome::Observed
                    });
            }
        }

        let Some(ctx) = self.dispatcher.as_ref() else {
            return ReactionOutcome::Observed; // watch-only
        };

        let request = ctx.dispatch_request(event);
        let decision = match ctx.dispatcher.dispatch(&request).await {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(error = %e, "dispatch failed");
                return ReactionOutcome::Observed;
            }
        };

        let Some(orchestrator) = self.orchestrator.as_ref() else {
            return ReactionOutcome::Decided(decision); // decided, nothing to execute with
        };

        match orchestrator
            .run(decision.clone(), &request.goal, &event.correlation_id)
            .await
        {
            // A proposal is an artifact the orchestrator built but cannot persist (no vault). Write
            // it here, with agent provenance so attribution suppresses the write (no self-reaction).
            Ok(Disposition::Propose(proposal)) => match self.write_proposal(&proposal).await {
                Ok(()) => ReactionOutcome::Acted(Disposition::Propose(proposal)),
                Err(e) => {
                    // Couldn't persist the artifact — surface the decision rather than claim it acted.
                    tracing::warn!(error = %e, "writing proposal failed");
                    ReactionOutcome::Decided(decision)
                }
            },
            Ok(disposition) => ReactionOutcome::Acted(disposition),
            Err(e) => {
                // Couldn't execute — surface the decision we did reach.
                tracing::warn!(error = %e, "orchestration failed");
                ReactionOutcome::Decided(decision)
            }
        }
    }

    /// Persist a proposal as a Markdown note under `proposals/`. Tagged with agent provenance for
    /// the daemon's own source, so the resulting change is attributed to us and not re-reacted to.
    /// The proposal's `id` (a correlation id with `:`/`/`) is slugified for the *filename* only —
    /// the authoritative id stays intact in the frontmatter for idempotency.
    async fn write_proposal(
        &self,
        proposal: &liberado_common::Proposal,
    ) -> Result<(), DaemonError> {
        let path = format!("proposals/{}.md", slugify(&proposal.id));
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
            if let Err(e) = notifier.notify(&message).await {
                // Best-effort — see RiskGatedToolRuntime::write_proposal's identical reasoning.
                tracing::warn!(error = %e, "failed to send proposal notification");
            }
        }

        Ok(())
    }

    /// A human edited a note under `proposals/`. If it is an APPROVED, non-expired, non-terminal
    /// proposal, execute its action and flip it to `done`. Anything else (still pending, rejected,
    /// expired, already done, or not a parseable proposal) is observed and left alone.
    async fn handle_proposal_change(
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

        // 3. Terminal states are never re-executed (at-most-once journal marker, Decision 6).
        if proposal.status.is_terminal() {
            tracing::debug!(status = ?proposal.status, "proposal is already terminal");
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

        // 6. Execute. An orchestration error is an infra failure and propagates (so it can be
        //    retried on the next watch cycle). We do NOT mark done on failure.
        let Some(orch) = self.orchestrator.as_ref() else {
            tracing::warn!("approved proposal but no orchestrator attached to execute it");
            return Ok(ReactionOutcome::Observed);
        };
        let report = orch.execute_approved(&proposal).await?;

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

        Ok(ReactionOutcome::Acted(Disposition::Reported(report)))
    }

    /// Run the watch loop until the watcher shuts down. Raw filesystem events are debounced per
    /// path (coalescing a notify burst into one settled change); each resulting external change is
    /// attributed, routed through the dispatcher (if attached), and the [`Reaction`] forwarded to
    /// `reactions`. Returns when the channel's receiver is dropped or the watcher closes.
    pub async fn run(self, reactions: UnboundedSender<Reaction>) -> Result<(), DaemonError> {
        let mut watch = self.vault.watch().await?;
        let mut debouncer = Debouncer::new(self.debounce);
        tracing::info!(
            vault = %self.vault.root().display(),
            debounce_ms = self.debounce.as_millis() as u64,
            "daemon watching vault"
        );

        loop {
            // Copy out the next deadline so the timer future borrows nothing from `debouncer`,
            // leaving the select arms free to mutate it.
            let next_deadline = debouncer.next_deadline();

            tokio::select! {
                maybe_event = watch.next_event() => {
                    let Some(event) = maybe_event else { break }; // watcher shut down
                    // Deletions carry no content to hash-join; reacting to them is a later iteration.
                    if let VaultEvent::FileDeleted(_) = event {
                        continue;
                    }
                    if let Some(rel) = self.vault.to_relative(event.path()) {
                        debouncer.observe(rel, Instant::now());
                    }
                }

                _ = sleep_until(next_deadline) => {
                    for rel in debouncer.drain_ready(Instant::now()) {
                        match self.process_change(&rel).await {
                            Ok(Some(event)) => {
                                let outcome = self.react(&event).await;
                                tracing::info!(
                                    path = event.payload.path.as_deref().unwrap_or_default(),
                                    outcome = outcome.label(),
                                    "reacting to external change"
                                );
                                if reactions.send(Reaction { event, outcome }).is_err() {
                                    return Ok(()); // receiver gone
                                }
                            }
                            Ok(None) => {} // our own write or vanished path — suppressed
                            Err(e) => tracing::warn!(error = %e, ?rel, "attribution failed"),
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Turn a correlation id into a single safe path segment for a proposal filename. Correlation ids
/// carry `:` and `/` (e.g. `vault-change:inbox/x.md:abc`), neither valid in a Windows filename and
/// the latter a directory separator — collapse every non-alphanumeric run to a single `-`.
fn slugify(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    let mut last_dash = false;
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Sleep until `deadline`, or forever when `None` (so the watch loop's select only wakes on
/// incoming events while nothing is pending).
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => {
            tokio::time::sleep(deadline.saturating_duration_since(Instant::now())).await
        }
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::{McpDescriptor, WriteProvenance};
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::mpsc::unbounded_channel;

    async fn temp_daemon() -> (Daemon, TempDir) {
        let dir = TempDir::new().unwrap();
        let daemon = Daemon::open("test", dir.path()).await.unwrap();
        (daemon, dir)
    }

    #[tokio::test]
    async fn external_change_produces_reaction() {
        let (daemon, dir) = temp_daemon().await;
        // A human writes a note directly (not through the adapter) — no matching audit entry.
        std::fs::write(dir.path().join("note.md"), "a human wrote this").unwrap();

        let event = daemon
            .process_change(Path::new("note.md"))
            .await
            .unwrap()
            .expect("external change should produce a reaction");
        assert_eq!(event.event_type, VAULT_NOTE_CHANGED);
        assert_eq!(event.source, event_source::TURBOVAULT_SUBSCRIPTION);
        assert_eq!(event.payload.path.as_deref(), Some("note.md"));
        assert!(event.is_reactable());
    }

    #[tokio::test]
    async fn our_own_write_is_suppressed() {
        let (daemon, _dir) = temp_daemon().await;
        let prov = WriteProvenance::agent("tasks-mcp", "c1");
        daemon
            .vault()
            .write("tasks/today.md", "- [ ] x", None, &prov)
            .await
            .unwrap();

        assert!(
            daemon
                .process_change(Path::new("tasks/today.md"))
                .await
                .unwrap()
                .is_none(),
            "agent write must not trigger a reaction"
        );
    }

    #[tokio::test]
    async fn missing_path_is_suppressed() {
        let (daemon, _dir) = temp_daemon().await;
        assert!(
            daemon
                .process_change(Path::new("nope.md"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn watcher_coalesces_burst_into_single_reaction() {
        let (daemon, dir) = temp_daemon().await;
        let daemon = daemon.with_debounce(Duration::from_millis(80));
        let vault_dir = dir.path().to_path_buf();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        // Give the watcher a moment to establish before writing.
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(vault_dir.join("captured.md"), "dropped in from Obsidian").unwrap();

        // Exactly one reaction, despite notify firing Create + Modify + ... for one write.
        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reaction")
            .expect("reaction channel closed");
        assert_eq!(reaction.event.payload.path.as_deref(), Some("captured.md"));
        assert!(
            matches!(reaction.outcome, ReactionOutcome::Observed),
            "watch-only: no dispatcher attached"
        );

        // No duplicate arrives within a generous margin past the debounce window.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            rx.try_recv().is_err(),
            "the notify burst should have coalesced into a single reaction"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn daemon_routes_reaction_through_dispatcher() {
        use liberado_config_loader::DispatchTuning;
        use liberado_common::{BlockReason, DispatchAction, DispatchDecision};
        use liberado_provider::{CompletionResponse, MockProvider};
        use std::sync::Arc;

        let (daemon, dir) = temp_daemon().await;

        // A dispatcher whose (mock) classifier returns a Clarify — the safe outcome with no MCPs.
        let canned = DispatchDecision {
            action: DispatchAction::Clarify {
                questions: vec!["how should I handle this note?".into()],
                what_blocked: BlockReason::Ambiguous,
            },
            confidence: 0.9,
            rationale: "test".into(),
        };
        let mock = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text(
                serde_json::to_string(&canned).unwrap(),
            )],
        ));
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_dispatcher(dispatcher, Arc::new(CapabilityCatalog::new()), CapabilitySet::empty());

        let vault_dir = dir.path().to_path_buf();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(vault_dir.join("idea.md"), "a captured thought").unwrap();

        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reaction")
            .expect("reaction channel closed");

        assert_eq!(reaction.event.payload.path.as_deref(), Some("idea.md"));
        // Dispatcher attached, but no orchestrator → decided, not acted.
        let ReactionOutcome::Decided(decision) = reaction.outcome else {
            panic!("expected Decided, got {}", reaction.outcome.label());
        };
        assert!(matches!(decision.action, DispatchAction::Clarify { .. }));

        handle.abort();
    }

    #[tokio::test]
    async fn daemon_acts_on_a_decision_via_the_orchestrator() {
        use liberado_config_loader::DispatchTuning;
        use liberado_common::{DispatchAction, DispatchDecision, Outcome};
        use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
        use liberado_orchestrator::Orchestrator;
        use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
        use std::sync::Arc;

        // A tool runtime + factory that need no real MCP (the scripted model just files a report).
        struct NoopRuntime;
        #[async_trait::async_trait]
        impl ToolRuntime for NoopRuntime {
            fn catalog(&self) -> Vec<ToolDef> {
                Vec::new()
            }
            async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
                Ok("ok".into())
            }
        }
        struct NoopFactory;
        #[async_trait::async_trait]
        impl RuntimeFactory for NoopFactory {
            async fn runtime_for(
                &self,
                _allowed_mcps: &[String],
                _provenance: liberado_common::WriteProvenance,
            ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
                Ok(Box::new(NoopRuntime))
            }
        }

        let (daemon, dir) = temp_daemon().await;

        // Dispatcher classifies to ExecuteDirect (no MCPs referenced → passes the guards).
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: Vec::new(),
            },
            confidence: 0.95,
            rationale: "trivial".into(),
        };
        let dispatch_provider = Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        ));
        let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

        // The orchestrator's executor: the model immediately files a report.
        let exec_provider = Arc::new(MockProvider::with_script(
            "exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "done" }),
            )])],
        ));
        let orchestrator = Orchestrator::new(
            exec_provider,
            NoopFactory,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            liberado_common::ProposalSigner::random(),
        );

        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_dispatcher(dispatcher, Arc::new(CapabilityCatalog::new()), CapabilitySet::empty())
            .with_orchestrator(orchestrator);

        let vault_dir = dir.path().to_path_buf();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(vault_dir.join("act.md"), "do something").unwrap();

        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");

        let ReactionOutcome::Acted(Disposition::Reported(report)) = reaction.outcome else {
            panic!("expected Acted/Reported, got {}", reaction.outcome.label());
        };
        assert_eq!(report.outcome, Outcome::Succeeded);

        handle.abort();
    }

    #[tokio::test]
    async fn daemon_emits_a_proposal_for_a_high_consequence_action() {
        use liberado_config_loader::DispatchTuning;
        use liberado_common::{
            Capability, Consequence, DispatchAction, DispatchDecision, Proposal, ProposalStatus,
            ProposedAction, ToolCall,
        };
        use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};
        use liberado_orchestrator::Orchestrator;
        use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
        use std::sync::Arc;

        // The orchestrator never builds a runtime for a Propose; this factory just satisfies the type.
        struct UnusedRuntime;
        #[async_trait::async_trait]
        impl ToolRuntime for UnusedRuntime {
            fn catalog(&self) -> Vec<ToolDef> {
                Vec::new()
            }
            async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
                Ok("ok".into())
            }
        }
        struct UnusedFactory;
        #[async_trait::async_trait]
        impl RuntimeFactory for UnusedFactory {
            async fn runtime_for(
                &self,
                _allowed_mcps: &[String],
                _provenance: WriteProvenance,
            ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
                Ok(Box::new(UnusedRuntime))
            }
        }

        let (daemon, dir) = temp_daemon().await;

        // Classifier picks a concrete external action: an ExecuteDirect with an `email:send` seed
        // call. Granted + confident, but External → the consequence gate downgrades it to Propose.
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: vec![ToolCall {
                    tool: "email:send".into(),
                    args: serde_json::json!({ "to": "boss@example.com" }),
                }],
                relevant_mcps: Vec::new(),
            },
            confidence: 0.95,
            rationale: "send the requested email".into(),
        };
        let dispatch_provider = Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        ));
        let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

        // Catalog declares the External MCP; capabilities grant it so the only block is consequence.
        let catalog = CapabilityCatalog::new();
        catalog.register(McpDescriptor {
            name: "email".into(),
            description: "send email".into(),
            consequence: Consequence::External,
            provenance: None,
            ..Default::default()
        });
        let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("email".into())]);
        let orchestrator = Orchestrator::new(
            Arc::new(MockProvider::with_script("exec", Vec::new())),
            UnusedFactory,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            liberado_common::ProposalSigner::random(),
        );

        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_dispatcher(dispatcher, Arc::new(catalog), capabilities)
            .with_orchestrator(orchestrator);

        let vault_dir = dir.path().to_path_buf();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(vault_dir.join("email-me.md"), "please email the boss").unwrap();

        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");

        let ReactionOutcome::Acted(Disposition::Propose(proposal)) = reaction.outcome else {
            panic!("expected Acted/Propose, got {}", reaction.outcome.label());
        };

        // The proposal artifact landed in the vault and round-trips back to a Pending proposal
        // carrying the email tool call.
        let proposals_dir = vault_dir.join("proposals");
        let entries: Vec<_> = std::fs::read_dir(&proposals_dir)
            .expect("proposals/ should exist")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one proposal note");
        let contents = std::fs::read_to_string(&entries[0]).unwrap();
        let parsed = Proposal::from_note(&contents).expect("proposal note round-trips");
        assert_eq!(parsed, proposal);
        assert_eq!(parsed.status, ProposalStatus::Pending);
        match parsed.proposed_action {
            ProposedAction::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].tool, "email:send");
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }

        handle.abort();
    }

    #[tokio::test]
    async fn daemon_executes_an_approved_proposal() {
        use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
        use liberado_orchestrator::Orchestrator;
        use liberado_provider::MockProvider;
        use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

        let runtime = InvocationRecordingRuntime::default();
        let invoked = runtime.invoked.clone();
        let signer = ProposalSigner::random();
        let orch = Orchestrator::new(
            Arc::new(MockProvider::with_script(
                "mock",
                Vec::<liberado_provider::CompletionResponse>::new(),
            )),
            InvocationRecordingFactory { runtime },
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            signer.clone(),
        );

        let (daemon, dir) = temp_daemon().await;
        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_orchestrator(orch)
            .with_proposal_signer(signer.clone());

        // Pre-create proposals/ so the watcher doesn't react to directory creation.
        let proposals_dir = dir.path().join("proposals");
        std::fs::create_dir_all(&proposals_dir).unwrap();

        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        // Let the watcher establish before writing the proposal file.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Write an approved proposal (simulates a human editing status to approved), signed with
        // the same key the daemon verifies against.
        let mut proposal = Proposal::pending(
            "vault-change:test-proposal:abc",
            "vault-change:test-proposal:abc",
            "test",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "tasks:create".into(),
                args: serde_json::json!({ "summary": "test task" }),
            }]),
            "a test proposal",
        );
        signer.sign(&mut proposal);
        proposal.status = ProposalStatus::Approved;
        std::fs::write(proposals_dir.join("approved.md"), &proposal.to_note()).unwrap();

        // Wait for the daemon to react to the proposal change.
        let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reaction")
            .expect("reaction channel closed");

        // (a) The runtime recorded the approved tool invocation.
        let recorded = invoked.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "approved proposal must execute the tool call"
        );
        assert_eq!(recorded[0].name, "tasks:create");

        // (b) The proposal note was flipped to Done.
        let contents = std::fs::read_to_string(proposals_dir.join("approved.md")).unwrap();
        let parsed = Proposal::from_note(&contents).unwrap();
        assert_eq!(parsed.status, ProposalStatus::Done);

        handle.abort();
    }

    #[tokio::test]
    async fn daemon_does_not_execute_a_pending_proposal() {
        use liberado_common::{Proposal, ProposalStatus, ProposedAction, ToolCall};
        use liberado_orchestrator::Orchestrator;
        use liberado_provider::MockProvider;
        use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

        let runtime = InvocationRecordingRuntime::default();
        let invoked = runtime.invoked.clone();
        let orch = Orchestrator::new(
            std::sync::Arc::new(MockProvider::with_script(
                "mock",
                Vec::<liberado_provider::CompletionResponse>::new(),
            )),
            InvocationRecordingFactory { runtime },
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            liberado_common::ProposalSigner::random(),
        );

        let (daemon, dir) = temp_daemon().await;
        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_orchestrator(orch);

        let proposals_dir = dir.path().join("proposals");
        std::fs::create_dir_all(&proposals_dir).unwrap();

        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(300)).await;

        // Write a PENDING proposal (not approved).
        let proposal = Proposal::pending(
            "pending-test",
            "pending-correlation",
            "test",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "tasks:create".into(),
                args: serde_json::json!({ "summary": "should-not-run" }),
            }]),
            "a pending proposal",
        );
        std::fs::write(proposals_dir.join("pending-test.md"), &proposal.to_note()).unwrap();

        // Wait for the daemon to process the change (reaction should arrive quickly).
        let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reaction")
            .expect("reaction channel closed");

        // Runtime recorded nothing — the pending proposal is not actionable.
        let recorded = invoked.lock().unwrap();
        assert!(
            recorded.is_empty(),
            "pending proposal must NOT invoke any tool"
        );

        // Proposal status is still Pending.
        let contents = std::fs::read_to_string(proposals_dir.join("pending-test.md")).unwrap();
        let parsed = Proposal::from_note(&contents).unwrap();
        assert_eq!(parsed.status, ProposalStatus::Pending);

        handle.abort();
    }

    #[tokio::test]
    async fn daemon_rejects_an_approved_proposal_with_a_bad_integrity_signature() {
        // Same shape as `daemon_executes_an_approved_proposal`, but the note is signed with a
        // DIFFERENT key than the daemon verifies against — simulating a wholesale-forged proposal
        // (or a legitimate one whose proposed_action was tampered with after signing). Must not
        // execute, and must not be marked done — left alone so a human can investigate.
        use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
        use liberado_orchestrator::Orchestrator;
        use liberado_provider::MockProvider;
        use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

        let runtime = InvocationRecordingRuntime::default();
        let invoked = runtime.invoked.clone();
        let daemon_signer = ProposalSigner::random();
        let orch = Orchestrator::new(
            Arc::new(MockProvider::with_script(
                "mock",
                Vec::<liberado_provider::CompletionResponse>::new(),
            )),
            InvocationRecordingFactory { runtime },
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            daemon_signer.clone(),
        );

        let (daemon, dir) = temp_daemon().await;
        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_orchestrator(orch)
            .with_proposal_signer(daemon_signer);

        let proposals_dir = dir.path().join("proposals");
        std::fs::create_dir_all(&proposals_dir).unwrap();

        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(300)).await;

        // Signed with an unrelated key, not the daemon's — a forged or tampered signature either
        // way, from the daemon's point of view.
        let forging_signer = ProposalSigner::random();
        let mut proposal = Proposal::pending(
            "forged-test",
            "forged-correlation",
            "test",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "tasks:create".into(),
                args: serde_json::json!({ "summary": "should-not-run" }),
            }]),
            "a forged approval",
        );
        forging_signer.sign(&mut proposal);
        proposal.status = ProposalStatus::Approved;
        std::fs::write(proposals_dir.join("forged.md"), &proposal.to_note()).unwrap();

        let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reaction")
            .expect("reaction channel closed");

        assert!(
            invoked.lock().unwrap().is_empty(),
            "a proposal with a bad integrity signature must NOT invoke any tool, even though \
             status is approved"
        );

        // Left as Approved (not silently flipped to Done) — a real failure state a human should
        // notice, not one indistinguishable from a successful run.
        let contents = std::fs::read_to_string(proposals_dir.join("forged.md")).unwrap();
        let parsed = Proposal::from_note(&contents).unwrap();
        assert_eq!(
            parsed.status,
            ProposalStatus::Approved,
            "a rejected-for-integrity proposal must not be marked Done"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn runtime_gated_downgrade_lands_in_the_vault_and_executes_once_approved() {
        // End-to-end proof of item 3's fix: a RiskGatedToolRuntime downgrade writes into the
        // *vault's* proposals/ directory (not a dead-end data dir), and approving it there actually
        // executes it via the same daemon pipeline pre-flight proposals already use.
        use liberado_common::{Capability, Consequence, Proposal, ProposalSigner, ProposalStatus};
        use liberado_executor::{RiskGatedToolRuntime, ToolRuntime};
        use liberado_orchestrator::Orchestrator;
        use liberado_provider::{MockProvider, ToolInvocation};
        use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

        let (daemon, dir) = temp_daemon().await;
        let vault_path = dir.path().to_path_buf();
        let signer = ProposalSigner::random();

        // 1. A RiskGatedToolRuntime downgrades a high-consequence call, writing a proposal straight
        //    into this vault's proposals/ directory (proposals_dir = the vault root — write_proposal
        //    joins "proposals" itself, matching the daemon's own PROPOSALS_DIR convention).
        let inner: Arc<dyn ToolRuntime> = Arc::new(InvocationRecordingRuntime::default());
        let gated = RiskGatedToolRuntime::new(
            inner,
            CapabilitySet::from_iter([Capability::ExecuteMcp("dangerous-mcp".into())]),
            vec![("dangerous-mcp".into(), Consequence::External)],
            Vec::new(),
            Vec::new(),
            vault_path.clone(),
            "clean up the vault".into(),
            "runtime-gate-test".into(),
            signer.clone(),
        );
        let call = ToolInvocation::new(
            "c1",
            "dangerous-mcp:wipe",
            serde_json::json!({ "path": "everything" }),
        );
        let downgrade_msg = gated.invoke(&call).await.expect("downgrade is Ok, not Err");
        assert!(downgrade_msg.contains("PROPOSAL CREATED"));

        let proposals_dir = vault_path.join("proposals");
        let mut entries: Vec<_> = std::fs::read_dir(&proposals_dir)
            .expect("the runtime-level downgrade must have created the vault's proposals/ dir")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(entries.len(), 1, "exactly one proposal file should exist");
        let proposal_path = entries.remove(0).path();
        let written = Proposal::from_note(&std::fs::read_to_string(&proposal_path).unwrap())
            .expect("proposal note round-trips");
        assert_eq!(written.status, ProposalStatus::Pending);
        assert!(signer.verify(&written), "the downgrade must be signed");

        // 2. Now wire up the daemon (matching signer + an orchestrator that records invocations) and
        //    let it observe a human's approval edit — the exact same pipeline pre-flight proposals
        //    already use, proving this is no longer a dead end.
        let runtime = InvocationRecordingRuntime::default();
        let invoked = runtime.invoked.clone();
        let orch = Orchestrator::new(
            Arc::new(MockProvider::with_script(
                "mock",
                Vec::<liberado_provider::CompletionResponse>::new(),
            )),
            InvocationRecordingFactory { runtime },
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            signer.clone(),
        );
        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_orchestrator(orch)
            .with_proposal_signer(signer);

        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));
        tokio::time::sleep(Duration::from_millis(300)).await;

        // The human approves — status flips, signature (over the unchanged action/id/etc.) stays
        // valid.
        let mut approved = written;
        approved.status = ProposalStatus::Approved;
        std::fs::write(&proposal_path, approved.to_note()).unwrap();

        let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reaction")
            .expect("reaction channel closed");

        let recorded = invoked.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "approving the runtime-gated proposal must actually execute it"
        );
        assert_eq!(recorded[0].name, "dangerous-mcp:wipe");

        handle.abort();
    }
}
