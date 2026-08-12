//! Shared daemon types, constants, and pool wiring.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use liberado_common::{
    CapabilityCatalog, CapabilitySet, DispatchDecision, Event, EventSource, ProposalSigner,
    UserTimezone, WriteClass,
};
use liberado_dispatcher::{DispatchRequest, Dispatcher};
use liberado_notify::Notifier;
use liberado_orchestrator::{Disposition, Orchestrator, OrchestratorError};
use liberado_session::GoalSessionHub;
use liberado_vault::{Vault, VaultError};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;

/// Default debounce window: long enough to coalesce a `notify` burst, short enough to feel live.
pub(crate) const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Default proposal reaper interval: sweep `proposals/` for expired entries every 10 minutes.
pub(crate) const DEFAULT_PROPOSAL_REAP_INTERVAL: Duration = Duration::from_secs(600);

/// Depth assigned to a daemon-originated reaction. It is the first agent step reacting to an
/// external change, so it starts the correlation chain at 1 (the depth cap halts longer cascades).
pub(crate) const DEFAULT_REACTION_DEPTH: u32 = 1;

/// Provenance source recorded for the daemon's own vault writes (e.g. a proposal artifact). Agent
/// provenance is what makes attribution suppress the write, so the daemon won't react to the
/// proposal it just wrote (loop-break, Decision 5).
pub(crate) const DAEMON_SOURCE: &str = "liberado";

/// Where resolved (terminal) proposal notes are filed once the daemon is done with them, so the
/// active `proposals/` dir doesn't silt up with a graveyard of `perm-…`/`prop-…` files. A per-
/// outcome subdirectory (`approved`/`rejected`/`expired`) is appended, making the folder self-
/// describing at a glance; the note's frontmatter still holds the authoritative status + scope.
/// `react` excludes this whole subtree so archived notes never re-enter the proposal pipeline.
pub(crate) const PROPOSALS_ARCHIVE_DIR: &str = "proposals/archive";

/// The `domain` recorded on a daemon reaction's background session (S5′ step 5).
///
/// A reaction is **not** run by a domain pack — the dispatcher classifies it and the orchestrator
/// executes it — so naming it `coding` or `life` would be a lie that a surface would then act on
/// (it would try to `/join` it as a steerable pack session). `dispatch` says what actually ran it.
/// Joining one of these is read-only: you watch what it did, you do not steer it.
pub(crate) const REACTION_DOMAIN: &str = "dispatch";

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
pub(crate) struct DispatcherContext {
    pub(crate) dispatcher: Dispatcher,
    /// Shared with the server's `/api/catalog` and (when attached) chat's own dispatch — one
    /// live source snapshotted fresh per request, not a copy frozen at construction time.
    pub(crate) catalog: Arc<CapabilityCatalog>,
    pub(crate) capabilities: CapabilitySet,
    pub(crate) reaction_depth: u32,
    /// `(zone, write_class)` pairs from `Policy.zones` — what the zone-write-class guard (§6 #2)
    /// checks a seed call's resolved target zone against.
    pub(crate) zone_write_classes: Vec<(String, WriteClass)>,
}

impl DispatcherContext {
    /// Turn an event (a vault change, a cron firing, a webhook POST — Decision 18/19, any attached
    /// [`liberado_common::EventSource`] or external producer via [`Daemon::event_sender`]) into a
    /// self-contained dispatch request. A vault change has a path to template a goal around; a
    /// non-vault trigger (`payload.path` absent — cron, webhook, or anything else) instead carries
    /// its configured goal directly in `payload.summary`.
    pub(crate) fn dispatch_request(&self, event: &Event) -> DispatchRequest {
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
            // M1b: routing catalog excludes peers marked degraded after connect/transport failure.
            catalog: self.catalog.routing_descriptors(),
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
/// `docs/future-work/ideas/a2a-protocol-idea.md`). Both halves stay independently optional, exactly mirroring
/// `Daemon`'s pre-pool fields, so `with_dispatcher`/`with_orchestrator` keep working regardless of
/// call order.
#[derive(Default)]
pub(crate) struct DaemonPool {
    pub(crate) dispatcher: Option<DispatcherContext>,
    pub(crate) orchestrator: Option<Orchestrator>,
}

/// The Liberado daemon.
pub struct Daemon {
    pub(crate) vault: Vault,
    pub(crate) debounce: Duration,
    /// How often (seconds) the background reaper sweeps `proposals/` for expired proposals.
    /// 0 disables the reaper. Defaults to 600 (10 minutes).
    pub(crate) proposal_reap_interval: Duration,
    /// Named dispatcher/executor pools, keyed by name. The `"default"` pool (`DEFAULT_POOL`) is
    /// what every event routed to before pools existed — `with_dispatcher`/`with_orchestrator`
    /// populate it and no other call site needs to change. Additional named pools are opt-in via
    /// `with_pool_dispatcher`/`with_pool_orchestrator`.
    pub(crate) pools: HashMap<String, DaemonPool>,
    /// Verifies a proposal's integrity signature before treating an approval edit as actionable
    /// (see `handle_proposal_change`). Defaults to a fresh random key at `open()` — production
    /// wiring overrides it via [`with_proposal_signer`](Self::with_proposal_signer) with the same
    /// installation-wide signer every proposal-creation site uses, so signatures actually match.
    pub(crate) signer: ProposalSigner,
    /// Where human approval decisions actually live — outside the vault, unreachable by any tool.
    ///
    /// `None` only in fixtures that never execute a proposal. In a built daemon it is always
    /// present, and its absence refuses rather than permits: see `handle_proposal_change`.
    pub(crate) approvals: Option<liberado_common::ApprovalLedger>,
    /// Told about every proposal this daemon writes (dispatcher pre-flight `Propose` path) —
    /// optional, `None` by default. Best-effort: a notification failure never blocks the write.
    pub(crate) notifier: Option<Arc<dyn Notifier>>,
    /// An additional event source run alongside the always-on vault watch (Decision 18/19) — e.g.
    /// `liberado-cron`'s `CronEventSource`. `None` by default: vault-watch is the only source, same
    /// as before this seam existed. At most one extra source for now (v1 scope); nothing prevents
    /// widening this to a `Vec` later if more than cron is ever attached simultaneously.
    pub(crate) cron_source: Option<Box<dyn EventSource>>,
    /// The shared sender every event source (vault-watch, cron, and external producers like
    /// `liberado-server`'s webhook receiver) pushes onto. Built once in `open()` — not per-`run()`
    /// call — specifically so [`event_sender`](Self::event_sender) can hand a clone to an external
    /// caller *before* `run` consumes `self`. `Some` until `run()` starts: `run()` `take()`s it (not
    /// just clones it) so `self`'s own reference is actually dropped once internal sources are
    /// spawned — otherwise the channel could never close on its own (an ever-alive sender inside
    /// `self` would keep `event_rx.recv()` from ever returning `None`).
    pub(crate) event_tx: Option<UnboundedSender<Event>>,
    /// Taken by `run()`; `None` after the daemon has started running once (it can only run once).
    pub(crate) event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<Event>>,
    /// Whether the vault watch task is actually alive.
    ///
    /// Built in `open()` — like `event_tx`, and for the same reason — so a surface can hold the
    /// same flag before `run()` consumes `self`. `run()` sets it once the watch task is spawned
    /// and clears it when that task ends, so a status endpoint reports what is true rather than a
    /// literal. The watch task is spawned detached, so without this nothing observes its death.
    pub(crate) watcher_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The **one** execution engine (one-execution-engine plan E3). When present, a reaction
    /// starts a hosted background session on this hub (domain `"dispatch"`) and returns
    /// [`ReactionOutcome::Dispatched`]. When absent, the daemon falls back to inline
    /// dispatch → orchestrate (no session recording) — useful for watch-only / unit tests.
    pub(crate) goals: Option<Arc<GoalSessionHub>>,
    /// Operator timezone ([`topology.timezone`](liberado_common::DEFAULT_TIMEZONE)). When set,
    /// non-vault triggers (cron, webhooks/wake-ups — anything without a vault `path`) get a
    /// "Local time: …" line prepended to the goal text so the model knows wall-clock without
    /// putting time in every system prompt. Vault-watch reactions are left alone.
    pub(crate) user_timezone: Option<UserTimezone>,
    /// Pre-resolved capability grants keyed by session profile name. When an event carries a
    /// `profile` in its `payload.data`, the reactor uses this grant (narrower or specialized
    /// vs the pool ceiling) — e.g. a cron electing `AskHuman` via a profile whose component
    /// includes it. Unattended crons omit `profile` and keep the pool grant (D-d).
    ///
    /// Populated by bootstrap from enabled `[[session_profiles]]` → `policy.capabilities_for`.
    /// An unknown / disabled profile name is **fail-closed**: the reaction is observed and no
    /// session is started (never silently falls back to full pool caps).
    pub(crate) session_profile_caps: HashMap<String, CapabilitySet>,
    /// Vault-relative glob patterns that the watcher must never react to (Syncthing conflict
    /// files, editor temp files, `Inbox/` when a dedicated schedule already processes it, etc.).
    /// Matched *before* attribution, so a matching path produces no event at all — the same
    /// `Ok(None)` as an agent-authored write. An empty list is a no-op (every path is checked).
    /// Populated from `[tuning.capture].inbox_ignore_globs`.
    pub(crate) inbox_ignore_globs: Vec<String>,
}
