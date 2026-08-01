//! # liberado-daemon
//!
//! The long-running core of Liberado (Decision 2: daemon-first). This is the v1 **vertical
//! slice**: it watches the vault, attributes every observed change (loop-breaking, Decision 5),
//! and forwards the changes that came from *outside* our own write path as standardized
//! [`Event`]s. Downstream (the dispatcher, hooks) consume those events; here we just produce them.
//!
//! The reactive decision is split into a pure, deterministic [`Daemon::process_change`] (testable
//! without the filesystem) and the watcher plumbing in [`Daemon::run`] - mirroring how the vault
//! crate separates attribution from I/O.
//!
//! Lifecycle modules: types, react, proposals, helpers (plus existing debounce / vault_source).

mod debounce;
mod helpers;
mod proposals;
mod react;
mod types;
mod vault_source;

pub use types::{Daemon, DaemonError, Reaction, ReactionOutcome, VAULT_NOTE_CHANGED};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use liberado_common::{
    CapabilityCatalog, CapabilitySet, DEFAULT_POOL, Event, EventSource, ProposalSigner,
    UserTimezone, WriteClass,
};
use liberado_dispatcher::Dispatcher;
use liberado_notify::Notifier;
use liberado_orchestrator::Orchestrator;
use liberado_session::GoalSessionHub;
use liberado_vault::Vault;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use vault_source::VaultEventSource;

use proposals::proposal_reap_loop;
use types::{
    DEFAULT_DEBOUNCE, DEFAULT_PROPOSAL_REAP_INTERVAL, DEFAULT_REACTION_DEPTH, DispatcherContext,
};

impl Daemon {
    /// Open the daemon over the vault at `vault_path` (enables the audit log).
    pub async fn open(
        name: impl Into<String>,
        vault_path: impl Into<PathBuf>,
    ) -> Result<Self, DaemonError> {
        let (event_tx, event_rx) = unbounded_channel::<Event>();
        Ok(Self {
            vault: Vault::open(name, vault_path).await?,
            debounce: DEFAULT_DEBOUNCE,
            proposal_reap_interval: DEFAULT_PROPOSAL_REAP_INTERVAL,
            session_profile_caps: HashMap::new(),
            pools: HashMap::new(),
            signer: ProposalSigner::random(),
            approvals: None,
            notifier: None,
            cron_source: None,
            event_tx: Some(event_tx),
            event_rx: Some(event_rx),
            goals: None,
            user_timezone: None,
        })
    }

    /// Set the operator timezone used to stamp local wall-clock onto cron/webhook goals.
    /// Production wiring: `config.topology.user_timezone()` via bootstrap.
    pub fn with_user_timezone(mut self, tz: UserTimezone) -> Self {
        self.user_timezone = Some(tz);
        self
    }

    /// How often the background reaper sweeps `proposals/` for expired proposals. 0 disables.
    /// Default is 600s (10 minutes). Set in `tuning.toml` via `proposals.reap_interval_secs`.
    pub fn with_proposal_reap_interval(mut self, secs: u64) -> Self {
        self.proposal_reap_interval = Duration::from_secs(secs);
        self
    }

    /// Configured proposal reaper stroke interval (`Duration::ZERO` means disabled).
    pub fn proposal_reap_interval(&self) -> Duration {
        self.proposal_reap_interval
    }

    /// Pre-resolved capability grants keyed by session profile name (enabled
    /// `[[session_profiles]]` → `policy.capabilities_for` at bootstrap). When an event carries a
    /// `profile`, the reactor uses that grant; an unknown name is fail-closed (no session).
    pub fn with_session_profile_caps(mut self, caps: HashMap<String, CapabilitySet>) -> Self {
        self.session_profile_caps = caps;
        self
    }

    /// The timezone attached via [`with_user_timezone`](Self::with_user_timezone), if any.
    pub fn user_timezone(&self) -> Option<UserTimezone> {
        self.user_timezone
    }

    /// Route every reaction through the **goal session hub** as a hosted background session
    /// (one-execution-engine plan E3).
    ///
    /// The hub must have the `"dispatch"` pack registered (see `liberado-dispatch-pack`). Each
    /// reactable event becomes a joinable, cancellable session — not a read-only recording of work
    /// the hub never ran. The reaction outcome is [`ReactionOutcome::Dispatched`] with the session
    /// id; the pack itself narrates and finishes the session.
    pub fn with_goal_hub(mut self, hub: Arc<GoalSessionHub>) -> Self {
        self.goals = Some(hub);
        self
    }

    /// A clone of the sender every event source pushes onto — the seam an external producer in the
    /// *same process* (e.g. `liberado-server`'s webhook HTTP handler) uses to inject an `Event`
    /// without needing its own `EventSource` loop. Grab this **before** calling
    /// [`run`](Self::run), which consumes `self` by value and takes the sender out.
    pub fn event_sender(&self) -> UnboundedSender<Event> {
        self.event_tx
            .as_ref()
            .expect("event_sender() called after run() already started")
            .clone()
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

    /// Attach an additional [`EventSource`] to run alongside the always-on vault watch (e.g.
    /// `liberado-cron`'s `CronEventSource`) — Decision 18/19's "cron and vault-watch are
    /// interchangeable event-sources" checkpoint. Optional; a daemon with none attached behaves
    /// exactly as before this seam existed (vault-watch only).
    pub fn with_cron_source(mut self, source: Box<dyn EventSource>) -> Self {
        self.cron_source = Some(source);
        self
    }

    /// Use `signer` to verify proposal integrity signatures, instead of the random ephemeral one
    /// `open()` generates by default. Production callers pass the same installation-wide signer
    /// every proposal-creation site (`Orchestrator`, `RiskGatedToolRuntime`) uses — see
    /// `liberado_bootstrap::configure_daemon`.
    /// Attach the ledger the daemon consults before executing an approved proposal.
    ///
    /// Without it, nothing executes: a proposal note saying `status: approved` is a claim, and the
    /// ledger is the only thing that corroborates it. A daemon built without one is a daemon that
    /// approves nothing, which is the correct direction for a missing security dependency.
    #[must_use]
    pub fn with_approval_ledger(mut self, ledger: liberado_common::ApprovalLedger) -> Self {
        self.approvals = Some(ledger);
        self
    }

    pub fn with_proposal_signer(mut self, signer: ProposalSigner) -> Self {
        self.signer = signer;
        self
    }

    /// Attach a dispatcher so reactable changes are routed to a [`DispatchDecision`]. Without one,
    /// the daemon runs in watch-only mode ([`ReactionOutcome::Observed`]). The `catalog` +
    /// `capabilities` form the disjoint context the dispatcher reasons over. `catalog` is the same
    /// shared, live `CapabilityCatalog` the server's API and (when attached) chat's own dispatch
    /// read — the daemon snapshots it fresh per reaction, not once at construction. `zone_write_classes`
    /// (`(zone, write_class)` pairs from `Policy.zones`) is what the zone-write-class guard (§6 #2)
    /// checks a seed call's resolved target zone against — taken here, not via a separate chained
    /// setter, so there is no call-order hazard: an earlier `with_zone_write_classes` builder method
    /// silently did nothing if called before this one (no `DispatcherContext` yet to attach to);
    /// folding it into this call's own parameters makes that ordering mistake impossible
    /// (`docs/roadmap/hygiene-audit-2026-07-05.md`).
    ///
    /// Attaches to the always-present `"default"` pool — every event that doesn't name a different
    /// pool (`EventPayload.pool`) routes here, exactly as if pools didn't exist. See
    /// [`with_pool_dispatcher`](Self::with_pool_dispatcher) to attach an *additional*, named pool
    /// (which does not take `zone_write_classes` — v1 additional pools don't yet have their own
    /// zone-write-class configuration; unchanged from before this fix).
    pub fn with_dispatcher(
        self,
        dispatcher: Dispatcher,
        catalog: Arc<CapabilityCatalog>,
        capabilities: CapabilitySet,
        zone_write_classes: Vec<(String, WriteClass)>,
    ) -> Self {
        let mut daemon = self.with_pool_dispatcher(DEFAULT_POOL, dispatcher, catalog, capabilities);
        if let Some(pool) = daemon.pools.get_mut(DEFAULT_POOL)
            && let Some(ctx) = &mut pool.dispatcher
        {
            ctx.zone_write_classes = zone_write_classes;
        }
        daemon
    }

    /// Attach a dispatcher to the named pool `name` (Decision 18 checkpoint #3) — creating the pool
    /// if it doesn't exist yet. See [`with_dispatcher`](Self::with_dispatcher) for the always-present
    /// `"default"` pool convenience wrapper over this (which also takes `zone_write_classes` — this
    /// method doesn't, since v1 additional pools have no zone-write-class configuration of their own
    /// yet).
    pub fn with_pool_dispatcher(
        mut self,
        name: impl Into<String>,
        dispatcher: Dispatcher,
        catalog: Arc<CapabilityCatalog>,
        capabilities: CapabilitySet,
    ) -> Self {
        self.pools.entry(name.into()).or_default().dispatcher = Some(DispatcherContext {
            dispatcher,
            catalog,
            capabilities,
            reaction_depth: DEFAULT_REACTION_DEPTH,
            zone_write_classes: Vec::new(),
        });
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
    ///
    /// Attaches to the always-present `"default"` pool. See
    /// [`with_pool_orchestrator`](Self::with_pool_orchestrator) for an additional, named pool.
    pub fn with_orchestrator(self, orchestrator: Orchestrator) -> Self {
        self.with_pool_orchestrator(DEFAULT_POOL, orchestrator)
    }

    /// Attach an orchestrator to the named pool `name` (Decision 18 checkpoint #3) — creating the
    /// pool if it doesn't exist yet. See [`with_orchestrator`](Self::with_orchestrator) for the
    /// always-present `"default"` pool convenience wrapper over this.
    pub fn with_pool_orchestrator(
        mut self,
        name: impl Into<String>,
        orchestrator: Orchestrator,
    ) -> Self {
        self.pools.entry(name.into()).or_default().orchestrator = Some(orchestrator);
        self
    }

    /// The underlying vault handle (cheap to clone).
    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    /// The signer this daemon verifies proposal approvals against (cheap to clone) — so an
    /// external actor on the same signature scheme (e.g. a Telegram approval bot) can flip a
    /// proposal's `status` using the identical key the daemon will check.
    pub fn signer(&self) -> &ProposalSigner {
        &self.signer
    }

    /// Run every attached [`EventSource`] (the always-on vault watch, plus any extra source like a
    /// `CronEventSource`) fanned into one channel, reacting to whatever arrives regardless of which
    /// source produced it — Decision 18/19's event-source seam. Each source runs its own loop in
    /// its own spawned task; this loop only ever sees the resulting [`Event`]s. Returns once every
    /// source has finished (or the `reactions` receiver is dropped).
    pub async fn run(mut self, reactions: UnboundedSender<Reaction>) -> Result<(), DaemonError> {
        let mut event_rx = self
            .event_rx
            .take()
            .expect("Daemon::run must only be called once");
        // `take()`, not `clone()` — actually removing it from `self` so `self`'s own reference is
        // gone once we `drop` our local clone below (see the field's doc comment).
        let event_tx = self
            .event_tx
            .take()
            .expect("Daemon::run must only be called once");

        let vault_source = VaultEventSource::new(self.vault.clone(), self.debounce);
        tokio::spawn(Box::new(vault_source).run(event_tx.clone()));

        if let Some(cron_source) = self.cron_source.take() {
            tracing::info!(
                source = cron_source.name(),
                "starting additional event source"
            );
            tokio::spawn(cron_source.run(event_tx.clone()));
        }

        if !self.proposal_reap_interval.is_zero() {
            let reap_interval = self.proposal_reap_interval;
            let reap_vault = self.vault.clone();
            tracing::info!(
                interval_secs = reap_interval.as_secs(),
                "starting proposal expiry reaper"
            );
            tokio::spawn(async move {
                proposal_reap_loop(reap_vault, reap_interval).await;
            });
        }

        // Drop our own clone so the channel closes once every spawned source — and any external
        // producer holding a clone via `event_sender()` — has finished. Otherwise `event_rx.recv()`
        // would wait forever even after every source exits.
        drop(event_tx);

        while let Some(event) = event_rx.recv().await {
            let outcome = self.react(&event).await;
            tracing::info!(
                source = %event.source,
                path = event.payload.path.as_deref().unwrap_or_default(),
                outcome = outcome.label(),
                "reacting to external event"
            );
            if reactions.send(Reaction { event, outcome }).is_err() {
                return Ok(()); // receiver gone
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests;
