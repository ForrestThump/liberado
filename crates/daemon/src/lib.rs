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
mod vault_source;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use liberado_common::{
    CapabilityCatalog, CapabilitySet, DEFAULT_POOL, DispatchDecision, Event, EventSource,
    PROPOSALS_DIR, ProposalSigner, SignedProposal, UserTimezone, WriteClass, event_source,
};
use liberado_dispatcher::{DispatchRequest, Dispatcher};
use liberado_notify::Notifier;
use liberado_orchestrator::{Disposition, Orchestrator, OrchestratorError};
use liberado_session::{
    DomainHint, GoalSessionHub, GoalSpec, SessionGrant, SessionOrigin, TerminalKind,
};
use liberado_vault::{Vault, VaultError};
use thiserror::Error;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use vault_source::VaultEventSource;

/// Default debounce window: long enough to coalesce a `notify` burst, short enough to feel live.
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Depth assigned to a daemon-originated reaction. It is the first agent step reacting to an
/// external change, so it starts the correlation chain at 1 (the depth cap halts longer cascades).
const DEFAULT_REACTION_DEPTH: u32 = 1;

/// Provenance source recorded for the daemon's own vault writes (e.g. a proposal artifact). Agent
/// provenance is what makes attribution suppress the write, so the daemon won't react to the
/// proposal it just wrote (loop-break, Decision 5).
const DAEMON_SOURCE: &str = "liberado";

/// The `domain` recorded on a daemon reaction's background session (S5′ step 5).
///
/// A reaction is **not** run by a domain pack — the dispatcher classifies it and the orchestrator
/// executes it — so naming it `coding` or `life` would be a lie that a surface would then act on
/// (it would try to `/join` it as a steerable pack session). `dispatch` says what actually ran it.
/// Joining one of these is read-only: you watch what it did, you do not steer it.
const REACTION_DOMAIN: &str = "dispatch";

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
    /// Still used for **proposal approvals**, which execute inline rather than as a session.
    Acted(Disposition),
    /// A goal session was started on the hub (one-execution-engine E3). The reaction feed is a
    /// *navigation surface* — open this session id to watch or join. Concurrent with other
    /// reactions; the session runs on its own.
    Dispatched { session_id: String },
}

impl ReactionOutcome {
    /// A short label for tracing.
    pub fn label(&self) -> &str {
        match self {
            ReactionOutcome::Observed => "(observed)",
            ReactionOutcome::Decided(d) => d.action.label(),
            ReactionOutcome::Acted(Disposition::Reported(_)) => "acted:reported",
            ReactionOutcome::Acted(Disposition::Clarify { .. }) => "acted:clarify",
            ReactionOutcome::Acted(Disposition::Propose(_)) => "acted:proposed",
            ReactionOutcome::Dispatched { .. } => "dispatched",
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
    /// Turn an event (a vault change, a cron firing, a webhook POST — Decision 18/19, any attached
    /// [`liberado_common::EventSource`] or external producer via [`Daemon::event_sender`]) into a
    /// self-contained dispatch request. A vault change has a path to template a goal around; a
    /// non-vault trigger (`payload.path` absent — cron, webhook, or anything else) instead carries
    /// its configured goal directly in `payload.summary`.
    fn dispatch_request(&self, event: &Event) -> DispatchRequest {
        let goal = match event.payload.path.as_deref() {
            Some(path) => format!(
                "A note in the vault was created or edited at '{path}'. Decide how to react to it."
            ),
            None => event
                .payload
                .summary
                .clone()
                .unwrap_or_else(|| "An event fired with no goal text configured.".to_string()),
        };
        DispatchRequest {
            goal,
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

/// A named dispatcher/executor pool (Decision 18 checkpoint #3): authority segregation only —
/// pools never communicate with each other (research-confirmed scope, see
/// `docs/ideas/a2a-protocol-idea.md`). Both halves stay independently optional, exactly mirroring
/// `Daemon`'s pre-pool fields, so `with_dispatcher`/`with_orchestrator` keep working regardless of
/// call order.
#[derive(Default)]
struct DaemonPool {
    dispatcher: Option<DispatcherContext>,
    orchestrator: Option<Orchestrator>,
}

/// The Liberado daemon.
pub struct Daemon {
    vault: Vault,
    debounce: Duration,
    /// Named dispatcher/executor pools, keyed by name. The `"default"` pool (`DEFAULT_POOL`) is
    /// what every event routed to before pools existed — `with_dispatcher`/`with_orchestrator`
    /// populate it and no other call site needs to change. Additional named pools are opt-in via
    /// `with_pool_dispatcher`/`with_pool_orchestrator`.
    pools: HashMap<String, DaemonPool>,
    /// Verifies a proposal's integrity signature before treating an approval edit as actionable
    /// (see `handle_proposal_change`). Defaults to a fresh random key at `open()` — production
    /// wiring overrides it via [`with_proposal_signer`](Self::with_proposal_signer) with the same
    /// installation-wide signer every proposal-creation site uses, so signatures actually match.
    signer: ProposalSigner,
    /// Told about every proposal this daemon writes (dispatcher pre-flight `Propose` path) —
    /// optional, `None` by default. Best-effort: a notification failure never blocks the write.
    notifier: Option<Arc<dyn Notifier>>,
    /// An additional event source run alongside the always-on vault watch (Decision 18/19) — e.g.
    /// `liberado-cron`'s `CronEventSource`. `None` by default: vault-watch is the only source, same
    /// as before this seam existed. At most one extra source for now (v1 scope); nothing prevents
    /// widening this to a `Vec` later if more than cron is ever attached simultaneously.
    cron_source: Option<Box<dyn EventSource>>,
    /// The shared sender every event source (vault-watch, cron, and external producers like
    /// `liberado-server`'s webhook receiver) pushes onto. Built once in `open()` — not per-`run()`
    /// call — specifically so [`event_sender`](Self::event_sender) can hand a clone to an external
    /// caller *before* `run` consumes `self`. `Some` until `run()` starts: `run()` `take()`s it (not
    /// just clones it) so `self`'s own reference is actually dropped once internal sources are
    /// spawned — otherwise the channel could never close on its own (an ever-alive sender inside
    /// `self` would keep `event_rx.recv()` from ever returning `None`).
    event_tx: Option<UnboundedSender<Event>>,
    /// Taken by `run()`; `None` after the daemon has started running once (it can only run once).
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<Event>>,
    /// The **one** execution engine (one-execution-engine plan E3). When present, a reaction
    /// starts a hosted background session on this hub (domain `"dispatch"`) and returns
    /// [`ReactionOutcome::Dispatched`]. When absent, the daemon falls back to inline
    /// dispatch → orchestrate (no session recording) — useful for watch-only / unit tests.
    goals: Option<Arc<GoalSessionHub>>,
    /// Operator timezone ([`topology.timezone`](liberado_common::DEFAULT_TIMEZONE)). When set,
    /// non-vault triggers (cron, webhooks/wake-ups — anything without a vault `path`) get a
    /// "Local time: …" line prepended to the goal text so the model knows wall-clock without
    /// putting time in every system prompt. Vault-watch reactions are left alone.
    user_timezone: Option<UserTimezone>,
}

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
            pools: HashMap::new(),
            signer: ProposalSigner::random(),
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

    /// The pure reactive decision: given an observed change to `rel_path`, return a reactable
    /// [`Event`], or `None` if the change was one of our own writes (suppressed by the hash-join)
    /// or the path is gone. No filesystem watching here — this is the unit-testable core. A thin
    /// wrapper over [`vault_source::attribute_and_build_event`], which [`VaultEventSource`]'s watch
    /// loop also calls — kept as its own public method so this stays directly testable without a
    /// filesystem, as before this crate had an `EventSource` seam.
    pub async fn process_change(&self, rel_path: &Path) -> Result<Option<Event>, DaemonError> {
        Ok(vault_source::attribute_and_build_event(&self.vault, rel_path).await?)
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

        // Which named pool (Decision 18 checkpoint #3) handles this event — the producer sets
        // `payload.pool` explicitly (cron/webhook); an unset pool (vault-watch, or anything that
        // doesn't opt in) routes to the always-present "default" pool.
        let pool_name = event.payload.pool.as_deref().unwrap_or(DEFAULT_POOL);
        let Some(pool) = self.pools.get(pool_name) else {
            tracing::warn!(
                pool = pool_name,
                "event names an unknown pool — observed only"
            );
            return ReactionOutcome::Observed;
        };

        let Some(ctx) = pool.dispatcher.as_ref() else {
            return ReactionOutcome::Observed; // watch-only
        };

        let mut request = ctx.dispatch_request(event);
        // Cron / webhook / wake-up: stamp local wall-clock so "today" / "this evening" means the
        // operator's zone without baking time into every system prompt. Vault-path reactions skip.
        if let Some(stamped) = self.stamp_local_time_if_needed(event, &request.goal) {
            request.goal = stamped;
        }

        // Preferred path (E3): start a hosted session on the hub. The dispatch pack classifies and
        // executes; this method returns as soon as the session exists. Reactions therefore run
        // concurrently with each other — no more awaiting one orchestrator before the next event.
        if let Some(hub) = &self.goals {
            let goal = reaction_goal(event, &request.goal, pool_name);
            let grant = SessionGrant {
                // Pool capabilities are the session's authority ceiling for this reaction. A
                // profile-narrowed cron (E7) would resolve a narrower grant here; without a profile
                // the pool is the grant. Crons default to no AskHuman via policy (D-d).
                capabilities: ctx.capabilities.clone(),
                profile: goal.profile.clone(),
                overrides: serde_json::Value::Null,
            };
            match hub.start_background(goal, grant).await {
                Ok(session_id) => {
                    tracing::info!(
                        %session_id,
                        pool = pool_name,
                        "reaction dispatched as hosted session"
                    );
                    // A background session normally stores its summary silently. A cron firing is
                    // the exception: it exists to hand that summary *back to the human*, so deliver
                    // a cron-sourced session's result through the notifier when it finishes.
                    self.maybe_deliver_cron_result(event, &session_id);
                    return ReactionOutcome::Dispatched { session_id };
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to start reaction session on hub");
                    return ReactionOutcome::Observed;
                }
            }
        }

        // Fallback: no hub attached — inline decide/act without a session (unit tests, watch tools).
        self.dispatch_and_act(pool, ctx, &request, event).await
    }

    /// Prepend "Local time: …" for non-vault triggers when a timezone is configured.
    fn stamp_local_time_if_needed(&self, event: &Event, goal: &str) -> Option<String> {
        if event.payload.path.is_some() {
            return None;
        }
        let tz = self.user_timezone?;
        // Prefer the event's own timestamp (the fire instant) so a delayed reaction still
        // reports the time that mattered for the schedule, not "whenever we got around to it".
        Some(tz.with_context_at(event.timestamp, goal))
    }

    /// Deliver a cron-fired session's result to the human via the notifier.
    ///
    /// Cron is the one background source whose whole purpose is to *report back* — a morning brief
    /// nobody sees is useless. Every other background session (a `delegate`d subagent, a vault-watch
    /// reaction) folds its outcome elsewhere and must stay silent, so this is gated strictly on the
    /// `cron:` event source. No notifier configured, or a non-cron source → no-op.
    ///
    /// Spawned onto the runtime rather than awaited inline: the reaction loop must not block on a
    /// session that may run for minutes. Best-effort delivery, matching every other `notify` call —
    /// a failed send is logged, never fatal. (A future per-schedule `deliver = false` opt-out would
    /// gate here; v1 delivers every cron.)
    fn maybe_deliver_cron_result(&self, event: &Event, session_id: &str) {
        let Some(schedule) = cron_schedule_name(&event.source) else {
            return;
        };
        let (Some(hub), Some(notifier)) = (self.goals.clone(), self.notifier.clone()) else {
            return;
        };
        let session_id = session_id.to_string();
        let schedule = schedule.to_string();
        tokio::spawn(async move {
            let snap = match hub.await_terminal(&session_id).await {
                Ok(snap) => snap,
                Err(e) => {
                    tracing::warn!(error = %e, schedule = %schedule, %session_id,
                        "cron session never reached terminal; nothing delivered");
                    return;
                }
            };
            let (summary, terminal) = snap
                .session
                .result
                .as_ref()
                .map(|r| (r.summary.clone(), r.terminal))
                .unwrap_or_else(|| {
                    (
                        "(the run finished with no summary)".to_string(),
                        TerminalKind::Failed,
                    )
                });
            let message = format_cron_delivery(&schedule, &summary, terminal);
            // `deliver_cron`, not `notify`: a chat-aware notifier folds this into the sticky
            // conversation and may defer it around the human's activity (see the Notifier trait).
            if let Err(e) = notifier.deliver_cron(&message).await {
                tracing::warn!(error = %e, schedule = %schedule, "cron result delivery failed");
            } else {
                tracing::info!(schedule = %schedule, %session_id, "cron result delivered");
            }
        });
    }

    /// Inline dispatch → orchestrate when no hub is attached. Prefer the hub path in production.
    async fn dispatch_and_act(
        &self,
        pool: &DaemonPool,
        ctx: &DispatcherContext,
        request: &DispatchRequest,
        event: &Event,
    ) -> ReactionOutcome {
        let decision = match ctx.dispatcher.dispatch(request).await {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(error = %e, "dispatch failed");
                return ReactionOutcome::Observed;
            }
        };

        let label = decision.action.label();

        let Some(orchestrator) = pool.orchestrator.as_ref() else {
            return ReactionOutcome::Decided(decision);
        };

        match orchestrator
            .run(
                decision.clone(),
                &request.goal,
                &event.correlation_id,
                &ctx.capabilities,
            )
            .await
        {
            Ok(Disposition::Propose(proposal)) => match self.write_proposal(&proposal).await {
                Ok(()) => ReactionOutcome::Acted(Disposition::Propose(proposal)),
                Err(e) => {
                    tracing::warn!(error = %e, "writing proposal failed");
                    ReactionOutcome::Decided(decision)
                }
            },
            Ok(disposition) => ReactionOutcome::Acted(disposition),
            Err(e) => {
                tracing::warn!(error = %e, "orchestration failed");
                let _ = label;
                ReactionOutcome::Decided(decision)
            }
        }
    }

    /// Persist a proposal as a Markdown note under `proposals/`. Tagged with agent provenance for
    /// the daemon's own source, so the resulting change is attributed to us and not re-reacted to.
    /// The proposal's `id` (a correlation id with `:`/`/`) is slugified for the *filename* only —
    /// the authoritative id stays intact in the frontmatter for idempotency.
    async fn write_proposal(&self, proposal: &SignedProposal) -> Result<(), DaemonError> {
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
    fn apply_approved_grant(&self, proposal: &liberado_common::Proposal) {
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
                let newly = liberado_common::session_grants::grant_for_session(pool, capability.clone());
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

/// The goal a reaction's hosted session records. `goal` is what the dispatcher was actually asked —
/// templated from the path for a vault change, the configured goal text for a cron or webhook — so
/// the session says what the reaction was *for*, not merely that one happened.
///
/// The event's `correlation_id` rides on `origin` with **no** parent conversation: nobody spawned a
/// cron from a chat, but it still belongs to a dispatch journal entry. That is the case
/// `SessionOrigin::from_correlation` exists for.
///
/// The schedule name iff `source` is a cron source (`"cron:{name}"`), else `None`.
///
/// The gate for cron result delivery: only a `cron:`-sourced reaction should have its summary pushed
/// to the human. A vault-watch reaction (`"turbovault-subscription"`, no `:name`) or a `delegate`d
/// subagent must never leak here, so the match is on the source *kind*, not a substring.
fn cron_schedule_name(source: &str) -> Option<&str> {
    match source.split_once(':') {
        Some((kind, name)) if kind == event_source::CRON => Some(name),
        _ => None,
    }
}

/// Render a cron result for delivery. On success it is just the brief under the schedule name; any
/// non-success terminal is tagged so a failed/exhausted run can never be mistaken for a real report
/// (the honest-status rule from `Disposition::terminal_summary`).
fn format_cron_delivery(schedule: &str, summary: &str, terminal: TerminalKind) -> String {
    if matches!(terminal, TerminalKind::Succeeded) {
        format!("🕒 {schedule}\n\n{summary}")
    } else {
        format!("🕒 {schedule} [{terminal:?}]\n\n{summary}")
    }
}

/// The `policy.toml` grant `component` whose capability ceiling gates a given pool — so an
/// "everywhere" grant lands where the blocked path actually reads its authority. The default pool's
/// ceiling is `capabilities_for("dispatcher")`; a named pool's is `capabilities_for(<pool name>)`
/// (see `liberado_bootstrap::configure_daemon`). A permission request stamps its owning pool onto
/// `Proposal.pool`, which is `Some(DEFAULT_POOL)` ("default") for the default pool.
fn grant_component_for_pool(pool: Option<&str>) -> &str {
    match pool {
        None | Some(DEFAULT_POOL) => "dispatcher",
        Some(name) => name,
    }
}

/// `pool` is stamped into `payload` so the dispatch pack routes to the same pool the event named.
fn reaction_goal(event: &Event, goal: &str, pool: &str) -> GoalSpec {
    let profile = event
        .payload
        .data
        .get("profile")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    GoalSpec {
        id: None,
        description: goal.to_string(),
        success_criteria: Vec::new(),
        domain: DomainHint::from(REACTION_DOMAIN),
        max_turns: 0,
        max_idle_secs: None,
        origin: Some(SessionOrigin::from_correlation(&event.correlation_id)),
        profile,
        payload: serde_json::json!({
            "source": event.source,
            "event_type": event.event_type,
            "path": event.payload.path,
            "pool": pool,
        }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::{McpDescriptor, WriteProvenance, event_source};
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn grant_component_maps_default_pool_to_the_dispatcher_ceiling() {
        // A permission request stamps the owning pool; the default pool's authority ceiling is the
        // "dispatcher" grant (see configure_daemon), so an "everywhere" grant must land there — not
        // on a literal "default" component that grants nothing.
        assert_eq!(grant_component_for_pool(None), "dispatcher");
        assert_eq!(grant_component_for_pool(Some(DEFAULT_POOL)), "dispatcher");
        // A named pool's ceiling is its own name.
        assert_eq!(grant_component_for_pool(Some("research")), "research");
    }

    #[test]
    fn cron_schedule_name_only_matches_cron_sources() {
        assert_eq!(
            cron_schedule_name("cron:daily-planning"),
            Some("daily-planning")
        );
        // Names may themselves contain colons (rfc3339-ish); only the first split matters.
        assert_eq!(
            cron_schedule_name("cron:weekly:review"),
            Some("weekly:review")
        );
        // Non-cron sources must never trigger delivery.
        assert_eq!(
            cron_schedule_name(event_source::TURBOVAULT_SUBSCRIPTION),
            None
        );
        assert_eq!(cron_schedule_name("delegate"), None);
        assert_eq!(cron_schedule_name("cronies:x"), None); // kind must equal "cron", not just prefix
        assert_eq!(cron_schedule_name("cron"), None); // no name
    }

    #[test]
    fn format_cron_delivery_flags_non_success() {
        let ok = format_cron_delivery("daily-planning", "your brief", TerminalKind::Succeeded);
        assert!(ok.contains("daily-planning") && ok.contains("your brief"));
        assert!(
            !ok.contains('['),
            "success must not carry a status tag: {ok}"
        );

        for bad in [
            TerminalKind::Failed,
            TerminalKind::Cancelled,
            TerminalKind::BudgetExhausted,
        ] {
            let msg = format_cron_delivery("daily-planning", "partial", bad);
            assert!(
                msg.contains(&format!("[{bad:?}]")),
                "non-success must be tagged so it isn't mistaken for a real report: {msg}"
            );
        }
    }

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
        use liberado_common::{BlockReason, DispatchAction, DispatchDecision};
        use liberado_config_loader::DispatchTuning;
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
            .with_dispatcher(
                dispatcher,
                Arc::new(CapabilityCatalog::new()),
                CapabilitySet::empty(),
                Vec::new(),
            );

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
    async fn cron_and_vault_watch_are_interchangeable_event_sources() {
        // The literal proof of Decision 18 checkpoint #3: a cron firing and a real vault change
        // both flow through the exact same `react()` path, over the exact same `Reaction` channel,
        // indistinguishable to the dispatcher except by `event.source`.
        use liberado_common::{BlockReason, DispatchAction, DispatchDecision};
        use liberado_config_loader::DispatchTuning;
        use liberado_cron::{CronEventSource, Schedule};
        use liberado_provider::{CompletionResponse, MockProvider};
        use std::collections::HashSet;
        use std::sync::Arc;

        let (daemon, dir) = temp_daemon().await;

        // Every reaction resolves to the same safe Clarify outcome — two reactions expected (one
        // per source), so the mock needs two scripted responses.
        let canned = DispatchDecision {
            action: DispatchAction::Clarify {
                questions: vec!["how should I handle this?".into()],
                what_blocked: BlockReason::Ambiguous,
            },
            confidence: 0.9,
            rationale: "test".into(),
        };
        let canned_json = serde_json::to_string(&canned).unwrap();
        let mock = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::text(canned_json.clone()),
                CompletionResponse::text(canned_json),
            ],
        ));
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let cron_source = CronEventSource::new(vec![Schedule {
            name: "every-second".into(),
            cron_expr: "* * * * * * *".into(),
            goal: "a cron-dispatched goal".into(),
            pool: None,
            profile: None,
        }])
        .unwrap();

        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_dispatcher(
                dispatcher,
                Arc::new(CapabilityCatalog::new()),
                CapabilitySet::empty(),
                Vec::new(),
            )
            .with_cron_source(Box::new(cron_source));

        let vault_dir = dir.path().to_path_buf();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(vault_dir.join("idea.md"), "a captured thought").unwrap();

        let mut sources = HashSet::new();
        for _ in 0..2 {
            let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out waiting for a reaction")
                .expect("reaction channel closed");
            assert!(matches!(reaction.outcome, ReactionOutcome::Decided(_)));
            sources.insert(reaction.event.source);
        }

        assert!(sources.contains(event_source::TURBOVAULT_SUBSCRIPTION));
        assert!(sources.contains("cron:every-second"));

        handle.abort();
    }

    #[tokio::test]
    async fn a_cron_firing_is_recorded_as_a_background_session_instead_of_vanishing() {
        // S5′ step 5, the whole point: before this, a cron fired, a model was called, the vault was
        // possibly written to — and the only trace was a log line. Nothing to see, nothing to join,
        // nothing to review. Now the reaction *is* a session, in the same store as your chats.
        use liberado_common::{DispatchAction, DispatchDecision};
        use liberado_config_loader::DispatchTuning;
        use liberado_cron::{CronEventSource, Schedule};
        use liberado_dispatch_pack::{DISPATCH_DOMAIN, DispatchPack};
        use liberado_executor::{
            RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime,
        };
        use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
        use liberado_session::{
            GoalSessionHub, GoalSessionStore, SessionEventKind, SessionStatus, TerminalKind,
            Visibility,
        };
        use std::sync::Arc;

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
                _provenance: WriteProvenance,
            ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
                Ok(Box::new(NoopRuntime))
            }
        }

        let (daemon, _dir) = temp_daemon().await;

        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: Vec::new(),
            },
            confidence: 0.95,
            rationale: "a nightly summary is routine".into(),
        };
        let dispatcher = Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "dispatch",
                [CompletionResponse::text(
                    serde_json::to_string(&decision).unwrap(),
                )],
            )),
            DispatchTuning::default(),
            4,
        );
        let orchestrator = Orchestrator::new(
            Arc::new(MockProvider::with_script(
                "exec",
                [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "c",
                    SUBMIT_REPORT_TOOL,
                    serde_json::json!({ "outcome": "succeeded", "summary": "summarized today" }),
                )])],
            )),
            NoopFactory,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            liberado_common::ProposalSigner::random(),
            "default",
        );

        let pack = DispatchPack::new(
            Arc::new(CapabilityCatalog::new()),
            Vec::new(),
            1,
            std::env::temp_dir(),
        )
        .with_pool("default", dispatcher, orchestrator);
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(pack));
        let hub = Arc::new(hub);

        let cron = CronEventSource::new(vec![Schedule {
            name: "nightly".into(),
            cron_expr: "* * * * * * *".into(), // every second, so the test doesn't wait
            goal: "summarize today's decisions".into(),
            pool: None,
            profile: None,
        }])
        .unwrap();

        // Hub path only needs a dispatcher context for the pool name / grant; the pack owns the
        // actual classify+execute engines. Attach a dummy pool dispatcher so pool lookup succeeds.
        let grant_dispatcher = Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "unused",
                Vec::<CompletionResponse>::new(),
            )),
            DispatchTuning::default(),
            4,
        );
        let daemon = daemon
            .with_dispatcher(
                grant_dispatcher,
                Arc::new(CapabilityCatalog::new()),
                CapabilitySet::empty(),
                Vec::new(),
            )
            .with_cron_source(Box::new(cron))
            .with_goal_hub(hub.clone());

        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for the cron reaction")
            .expect("reaction channel closed");
        assert_eq!(reaction.event.source, "cron:nightly");
        let session_id = match reaction.outcome {
            ReactionOutcome::Dispatched { session_id } => session_id,
            other => panic!("expected Dispatched, got {}", other.label()),
        };
        assert_eq!(
            hub.snapshot(&session_id)
                .await
                .unwrap()
                .session
                .goal
                .domain
                .as_str(),
            DISPATCH_DOMAIN
        );

        // Session runs concurrently — wait for terminal.
        let snap = hub.await_terminal(&session_id).await.expect("terminal");
        let row = &snap.session;

        // Nobody was watching — and the record says so, which is what `Visibility` is *for*.
        assert_eq!(row.visibility, Visibility::Background);
        // It says what it was for, not merely that something happened.
        assert_eq!(row.goal.description, "summarize today's decisions");
        // Tied back to the dispatch journal, with no parent conversation — a cron has none.
        let origin = row
            .goal
            .origin
            .as_ref()
            .expect("origin carries correlation");
        assert!(
            origin
                .correlation_id
                .as_deref()
                .unwrap()
                .starts_with("cron:nightly:")
        );
        assert!(origin.conversation_id.is_none());
        // It ran to a real terminal state carrying the executor's own report.
        assert_eq!(row.status, SessionStatus::Succeeded);
        assert_eq!(row.result.as_ref().unwrap().summary, "summarized today");
        assert_eq!(
            row.result.as_ref().unwrap().terminal,
            TerminalKind::Succeeded
        );

        // And it left a readable transcript, not just a status.
        let events = &snap.events;
        assert!(matches!(
            events.first().map(|e| &e.kind),
            Some(SessionEventKind::SessionStarted { .. })
        ));
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                SessionEventKind::Progress { message }
                    if message.contains("ExecuteDirect")
                        && message.contains("a nightly summary is routine")
            )),
            "the dispatch decision and its rationale should be narrated into the transcript: \
             {events:#?}"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn a_reaction_that_needed_a_human_fails_honestly_rather_than_reporting_success() {
        // A background session's status is the only thing a human sees at a glance. A Clarify means
        // the reaction stopped and asked a question nobody was there to answer — so it must not be
        // green. The unanswered questions have to survive into the summary, or they're lost.
        use liberado_common::{BlockReason, DispatchAction, DispatchDecision};
        use liberado_config_loader::DispatchTuning;
        use liberado_dispatch_pack::DispatchPack;
        use liberado_provider::{CompletionResponse, MockProvider};
        use liberado_session::{GoalSessionHub, GoalSessionStore, SessionStatus, Visibility};
        use std::sync::Arc;

        let (daemon, _dir) = temp_daemon().await;

        let canned = DispatchDecision {
            action: DispatchAction::Clarify {
                questions: vec!["which project did you mean?".into()],
                what_blocked: BlockReason::Ambiguous,
            },
            confidence: 0.9,
            rationale: "ambiguous".into(),
        };
        let dispatcher = Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "dispatch",
                [CompletionResponse::text(
                    serde_json::to_string(&canned).unwrap(),
                )],
            )),
            DispatchTuning::default(),
            4,
        );
        let orchestrator = Orchestrator::new(
            Arc::new(MockProvider::with_script("exec", Vec::new())),
            NoopFactoryForClarify,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            liberado_common::ProposalSigner::random(),
            "default",
        );

        let pack = DispatchPack::new(
            Arc::new(CapabilityCatalog::new()),
            Vec::new(),
            1,
            std::env::temp_dir(),
        )
        .with_pool("default", dispatcher, orchestrator);
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(pack));
        let hub = Arc::new(hub);

        let grant_dispatcher = Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "unused",
                Vec::<CompletionResponse>::new(),
            )),
            DispatchTuning::default(),
            4,
        );
        let daemon = daemon
            .with_dispatcher(
                grant_dispatcher,
                Arc::new(CapabilityCatalog::new()),
                CapabilitySet::empty(),
                Vec::new(),
            )
            .with_goal_hub(hub.clone());

        let sender = daemon.event_sender();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(200)).await;
        sender
            .send(Event::trigger(
                "WebhookFired",
                "webhook:nightly",
                "webhook:nightly:1",
                liberado_common::EventPayload {
                    summary: Some("tidy up the inbox".into()),
                    ..Default::default()
                },
            ))
            .unwrap();

        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        let session_id = match reaction.outcome {
            ReactionOutcome::Dispatched { session_id } => session_id,
            other => panic!("expected Dispatched, got {}", other.label()),
        };
        let snap = hub.await_terminal(&session_id).await.unwrap();
        let row = &snap.session;
        // A webhook is just as unattended as a cron — same seam, same treatment.
        assert_eq!(row.visibility, Visibility::Background);
        assert_eq!(
            row.status,
            SessionStatus::Failed,
            "a reaction that could not proceed without a human did not succeed"
        );
        let summary = &row.result.as_ref().unwrap().summary;
        assert!(
            summary.contains("which project did you mean?"),
            "the unanswered question must survive into the summary, got: {summary}"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn a_reaction_whose_execution_blew_up_says_so_instead_of_blaming_a_missing_orchestrator()
    {
        // The pack's orchestration error must land in the session summary honestly — not as
        // "no orchestrator is attached". Hub path: reaction returns Dispatched immediately; the
        // pack fails the session with the real error.
        use liberado_common::{DispatchAction, DispatchDecision};
        use liberado_config_loader::DispatchTuning;
        use liberado_dispatch_pack::DispatchPack;
        use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};
        use liberado_provider::{CompletionResponse, MockProvider};
        use liberado_session::{GoalSessionHub, GoalSessionStore, SessionStatus};
        use std::sync::Arc;

        struct FailingFactory;
        #[async_trait::async_trait]
        impl RuntimeFactory for FailingFactory {
            async fn runtime_for(
                &self,
                _allowed_mcps: &[String],
                _provenance: WriteProvenance,
            ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
                Err(RuntimeSetupError(
                    "Connection failed: error sending request for url (http://127.0.0.1:3737/mcp)"
                        .into(),
                ))
            }
        }

        let (daemon, _dir) = temp_daemon().await;
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: Vec::new(),
            },
            confidence: 0.95,
            rationale: "routine".into(),
        };
        // Give the pool a grant so ExecuteDirect tries the factory (empty grant uses NoMcpRuntime).
        let pool_caps =
            CapabilitySet::from_iter([liberado_common::Capability::ExecuteMcp("x".into())]);
        let dispatcher = Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "dispatch",
                [CompletionResponse::text(
                    serde_json::to_string(&decision).unwrap(),
                )],
            )),
            DispatchTuning::default(),
            4,
        );
        let orchestrator = Orchestrator::new(
            Arc::new(MockProvider::with_script("exec", Vec::new())),
            FailingFactory,
            pool_caps.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            liberado_common::ProposalSigner::random(),
            "default",
        );

        let pack = DispatchPack::new(
            Arc::new(CapabilityCatalog::new()),
            Vec::new(),
            1,
            std::env::temp_dir(),
        )
        .with_pool("default", dispatcher, orchestrator);
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(pack));
        let hub = Arc::new(hub);

        let grant_dispatcher = Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "unused",
                Vec::<CompletionResponse>::new(),
            )),
            DispatchTuning::default(),
            4,
        );
        let daemon = daemon
            .with_dispatcher(
                grant_dispatcher,
                Arc::new(CapabilityCatalog::new()),
                pool_caps,
                Vec::new(),
            )
            .with_goal_hub(hub.clone());

        let sender = daemon.event_sender();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(200)).await;
        sender
            .send(Event::trigger(
                "CronFired",
                "cron:nightly",
                "cron:nightly:1",
                liberado_common::EventPayload {
                    summary: Some("say hello".into()),
                    ..Default::default()
                },
            ))
            .unwrap();
        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        let session_id = match reaction.outcome {
            ReactionOutcome::Dispatched { session_id } => session_id,
            other => panic!("expected Dispatched, got {}", other.label()),
        };
        let snap = hub.await_terminal(&session_id).await.unwrap();
        assert_eq!(snap.session.status, SessionStatus::Failed);
        let summary = &snap.session.result.as_ref().unwrap().summary;
        assert!(
            summary.contains("orchestration failed") || summary.contains("Connection failed"),
            "the summary must say execution was attempted and failed, got: {summary}"
        );
        assert!(
            !summary.contains("no orchestrator"),
            "an orchestrator IS attached — blaming a missing one sends the human hunting a \
             config bug that does not exist: {summary}"
        );

        handle.abort();
    }

    /// Never actually builds a runtime (the Clarify path stops before execution) — exists only to
    /// satisfy `Orchestrator::new`'s type.
    struct NoopFactoryForClarify;
    #[async_trait::async_trait]
    impl liberado_executor::RuntimeFactory for NoopFactoryForClarify {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn liberado_executor::ToolRuntime>, liberado_executor::RuntimeSetupError>
        {
            unreachable!("a Clarify never reaches execution")
        }
    }

    #[tokio::test]
    async fn pools_are_authority_segregated() {
        // The direct proof that named pools (Decision 18 checkpoint #3) aren't just routed but
        // actually authority-segregated: two pools, two schedules, both decisions asking to call
        // the SAME MCP — but only "granted-pool" was actually given that capability. If pools
        // shared authority (e.g. a bug reusing one capability set for both), "blocked-pool" would
        // reach the real runtime too; it must not.
        use liberado_common::{Capability, DispatchAction, DispatchDecision, EventPayload};
        use liberado_config_loader::DispatchTuning;
        use liberado_executor::SUBMIT_REPORT_TOOL;
        use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
        use liberado_test_support::CallRecordingFactory;

        let (daemon, _dir) = temp_daemon().await;

        // Both pools' dispatchers classify identically: ExecuteDirect against "shared-mcp".
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: vec!["shared-mcp".into()],
            },
            confidence: 0.9,
            rationale: "test".into(),
        };
        let decision_json = serde_json::to_string(&decision).unwrap();
        let dispatcher_for = || {
            Dispatcher::new(
                Arc::new(MockProvider::with_script(
                    "dispatch",
                    [CompletionResponse::text(decision_json.clone())],
                )),
                DispatchTuning::default(),
                4,
            )
        };

        // granted-pool: actually holds the "shared-mcp" capability, so its orchestrator's
        // ExecuteDirect scoping resolves a non-empty `allowed_mcps` and reaches the real factory.
        let granted_capabilities =
            CapabilitySet::from_iter([Capability::ExecuteMcp("shared-mcp".into())]);
        let granted_factory = CallRecordingFactory::default();
        let granted_calls = granted_factory.calls.clone();
        let granted_exec = Arc::new(MockProvider::with_script(
            "exec-granted",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "c1",
                    "shared-mcp:do_thing",
                    serde_json::json!({}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "c2",
                    SUBMIT_REPORT_TOOL,
                    serde_json::json!({ "outcome": "succeeded", "summary": "granted pool acted" }),
                )]),
            ],
        ));
        let granted_orch = Orchestrator::new(
            granted_exec,
            granted_factory,
            granted_capabilities.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            liberado_common::ProposalSigner::random(),
            "granted-pool",
        );

        // blocked-pool: an EMPTY capability set — the dispatcher's own pre-flight capability guard
        // (`guards::evaluate`'s `CapabilityGap` check, run against THIS pool's own capabilities)
        // catches the reference to "shared-mcp" before the decision ever reaches an orchestrator,
        // downgrading it to Clarify — so `blocked_exec`/`blocked_factory` below must NEVER be
        // touched at all. That's the segregation proof: the identical decision that runs for real
        // in granted-pool never even reaches execution in blocked-pool.
        let blocked_factory = CallRecordingFactory::default();
        let blocked_calls = blocked_factory.calls.clone();
        let blocked_exec = Arc::new(MockProvider::with_script("exec-blocked", []));
        let blocked_orch = Orchestrator::new(
            blocked_exec,
            blocked_factory,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            liberado_common::ProposalSigner::random(),
            "blocked-pool",
        );

        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_pool_dispatcher(
                "granted-pool",
                dispatcher_for(),
                Arc::new(CapabilityCatalog::new()),
                granted_capabilities,
            )
            .with_pool_orchestrator("granted-pool", granted_orch)
            .with_pool_dispatcher(
                "blocked-pool",
                dispatcher_for(),
                Arc::new(CapabilityCatalog::new()),
                CapabilitySet::empty(),
            )
            .with_pool_orchestrator("blocked-pool", blocked_orch);

        // Inject one event per pool directly (the same seam `liberado-server`'s webhook handler
        // and `liberado-cron` both use) — deterministic, no dependence on real-time cron ticking.
        let sender = daemon.event_sender();

        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(200)).await;
        sender
            .send(Event::trigger(
                "Trigger",
                "test:granted",
                "test:granted:1",
                EventPayload {
                    pool: Some("granted-pool".into()),
                    ..Default::default()
                },
            ))
            .unwrap();
        sender
            .send(Event::trigger(
                "Trigger",
                "test:blocked",
                "test:blocked:1",
                EventPayload {
                    pool: Some("blocked-pool".into()),
                    ..Default::default()
                },
            ))
            .unwrap();

        let mut outcomes_by_pool = std::collections::HashMap::new();
        for _ in 0..2 {
            let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out waiting for a reaction")
                .expect("reaction channel closed");
            outcomes_by_pool.insert(reaction.event.payload.pool.clone(), reaction.outcome);
        }

        // granted-pool: authorized for "shared-mcp" — the decision runs for real.
        match outcomes_by_pool.get(&Some("granted-pool".to_string())) {
            Some(ReactionOutcome::Acted(Disposition::Reported(_))) => {}
            Some(o) => panic!("expected granted-pool to reach Reported, got {}", o.label()),
            None => panic!("no reaction recorded for granted-pool"),
        }

        // blocked-pool: an identical decision naming the same MCP, but this pool was never
        // granted it — the dispatcher's own pre-flight guard catches it and downgrades to
        // Clarify, never reaching an orchestrator/runtime at all.
        match outcomes_by_pool.get(&Some("blocked-pool".to_string())) {
            Some(ReactionOutcome::Acted(Disposition::Clarify { what_blocked, .. })) => {
                assert_eq!(*what_blocked, liberado_common::BlockReason::CapabilityGap);
            }
            Some(o) => panic!(
                "expected blocked-pool to be guard-downgraded to Clarify, got {}",
                o.label()
            ),
            None => panic!("no reaction recorded for blocked-pool"),
        }

        // The load-bearing assertion: granted-pool's own capability actually reached the real
        // runtime; blocked-pool's identical request never did, despite an identical decision.
        assert_eq!(
            granted_calls.lock().unwrap().len(),
            1,
            "granted-pool must reach the real runtime for a call it's actually authorized for"
        );
        assert!(
            blocked_calls.lock().unwrap().is_empty(),
            "blocked-pool must NEVER reach the real runtime for an MCP it wasn't granted, even \
             though the decision asked for the exact same call granted-pool made"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn event_sender_lets_an_external_producer_inject_an_event() {
        // The seam `liberado-server`'s webhook handler uses: grab a sender before `run()` moves
        // `self`, then push an `Event` in from completely outside any `EventSource` — no cron, no
        // vault change, just a direct injection — and it must still flow through `react()` exactly
        // like any other source.
        use liberado_common::EventPayload;

        let (daemon, _dir) = temp_daemon().await;
        let sender = daemon.event_sender();

        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(200)).await;
        sender
            .send(Event::trigger(
                "WebhookFired",
                "webhook:test-hook",
                "webhook:test-hook:1",
                EventPayload {
                    summary: Some("an externally-injected goal".into()),
                    ..Default::default()
                },
            ))
            .unwrap();

        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for the injected event's reaction")
            .expect("reaction channel closed");

        assert_eq!(reaction.event.source, "webhook:test-hook");
        // No dispatcher attached in this test daemon — watch-only, so Observed — the point here is
        // only that the injected event reached `react()` at all, not what it decided.
        assert!(matches!(reaction.outcome, ReactionOutcome::Observed));

        handle.abort();
    }

    #[tokio::test]
    async fn daemon_acts_on_a_decision_via_the_orchestrator() {
        use liberado_common::{DispatchAction, DispatchDecision, Outcome};
        use liberado_config_loader::DispatchTuning;
        use liberado_executor::{
            RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime,
        };
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
            "default",
        );

        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_dispatcher(
                dispatcher,
                Arc::new(CapabilityCatalog::new()),
                CapabilitySet::empty(),
                Vec::new(),
            )
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
        use liberado_common::{
            Capability, Consequence, DispatchAction, DispatchDecision, Proposal, ProposalStatus,
            ProposedAction, ToolCall,
        };
        use liberado_config_loader::DispatchTuning;
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
            "default",
        );

        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_dispatcher(dispatcher, Arc::new(catalog), capabilities, Vec::new())
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
        assert_eq!(&parsed, proposal.as_proposal());
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
    async fn daemon_downgrades_a_zone_restricted_seed_call_to_a_proposal() {
        // End-to-end regression for the zone-write-class guard (§6 #2) at the daemon level: unit
        // tests already cover `dispatcher::guards::evaluate` and `RiskGatedToolRuntime` separately
        // (they share `liberado_common::zone_write_restriction` so they can't drift on the
        // determination logic itself), but nothing previously proved the daemon actually threads a
        // configured `zone_write_classes` through `with_dispatcher` into a real reaction
        // (`docs/roadmap/hygiene-audit-2026-07-05.md` P3.4). The MCP's own `consequence` is
        // `Reversible` — below the consequence gate — so if a proposal is emitted here, the zone
        // restriction is provably what caused it, not the (separately already-tested) consequence
        // check.
        use liberado_common::{
            Capability, Consequence, DispatchAction, DispatchDecision, Proposal, ProposalStatus,
            ProposedAction, ToolCall,
        };
        use liberado_config_loader::DispatchTuning;
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

        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: vec![ToolCall {
                    tool: "vault-mcp:write_note".into(),
                    args: serde_json::json!({ "path": "reviews/q1.md" }),
                }],
                relevant_mcps: Vec::new(),
            },
            confidence: 0.95,
            rationale: "file the review note".into(),
        };
        let dispatch_provider = Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        ));
        let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

        // Reversible (not Irreversible/External), granted by capability, and targets a zone this
        // pool has restricted to ProposalOnly — the only thing that should block direct execution.
        let catalog = CapabilityCatalog::new();
        catalog.register(McpDescriptor {
            name: "vault-mcp".into(),
            description: "vault note writer".into(),
            consequence: Consequence::Reversible,
            provenance: None,
            default_zone: Some("reviews".into()),
            tool_zones: Vec::new(),
            zone_from_arg: None,
            write_tools: Vec::new(),
        });
        let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("vault-mcp".into())]);
        let zone_write_classes = vec![("reviews".to_string(), WriteClass::ProposalOnly)];
        let orchestrator = Orchestrator::new(
            Arc::new(MockProvider::with_script("exec", Vec::new())),
            UnusedFactory,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            liberado_common::ProposalSigner::random(),
            "default",
        );

        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_dispatcher(
                dispatcher,
                Arc::new(catalog),
                capabilities,
                zone_write_classes,
            )
            .with_orchestrator(orchestrator);

        let vault_dir = dir.path().to_path_buf();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(vault_dir.join("review-me.md"), "please file this review").unwrap();

        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");

        let ReactionOutcome::Acted(Disposition::Propose(proposal)) = reaction.outcome else {
            panic!("expected Acted/Propose, got {}", reaction.outcome.label());
        };

        let proposals_dir = vault_dir.join("proposals");
        let entries: Vec<_> = std::fs::read_dir(&proposals_dir)
            .expect("proposals/ should exist")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one proposal note");
        let contents = std::fs::read_to_string(&entries[0]).unwrap();
        let parsed = Proposal::from_note(&contents).expect("proposal note round-trips");
        assert_eq!(&parsed, proposal.as_proposal());
        assert_eq!(parsed.status, ProposalStatus::Pending);
        match parsed.proposed_action {
            ProposedAction::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].tool, "vault-mcp:write_note");
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
            "default",
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
        let proposal = Proposal::pending(
            "vault-change:test-proposal:abc",
            "vault-change:test-proposal:abc",
            "test",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "tasks:create".into(),
                args: serde_json::json!({ "summary": "test task" }),
            }]),
            "a test proposal",
        );
        let mut proposal = signer.sign(proposal);
        proposal.set_status(ProposalStatus::Approved);
        std::fs::write(proposals_dir.join("approved.md"), proposal.to_note()).unwrap();

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
            "default",
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
        std::fs::write(proposals_dir.join("pending-test.md"), proposal.to_note()).unwrap();

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
            "default",
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
        let proposal = Proposal::pending(
            "forged-test",
            "forged-correlation",
            "test",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "tasks:create".into(),
                args: serde_json::json!({ "summary": "should-not-run" }),
            }]),
            "a forged approval",
        );
        let mut proposal = forging_signer.sign(proposal);
        proposal.set_status(ProposalStatus::Approved);
        std::fs::write(proposals_dir.join("forged.md"), proposal.to_note()).unwrap();

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
            "default",
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
            "default",
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
