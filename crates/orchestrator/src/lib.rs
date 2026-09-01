//! # liberado-orchestrator
//!
//! The bridge from a dispatcher [`DispatchDecision`] to an actual execution. Given a decision, it:
//!
//! - **Clarify** → returns it unexecuted (the main agent resolves it).
//! - **ExecuteDirect** / **DispatchSubagent** → builds the executor [`Task`] and the write
//!   [`WriteProvenance`], obtains a [`ToolRuntime`] from the injected [`RuntimeFactory`], runs the
//!   executor's adaptive agent loop, and returns the [`Report`].
//!
//! The provenance correlation is what closes the loop-break: every write a tool makes during the
//! execution is tagged with it, so the daemon attributes the resulting vault change to us and
//! suppresses it (validated in `liberado-vault`'s `provenance_e2e`). The correlation source differs
//! by action — `ExecuteDirect` adopts the **triggering** correlation (it acts in the reaction's
//! name), while `DispatchSubagent` uses the classifier-minted `correlation_id` it carries.
//!
//! Connection management (how a [`ToolRuntime`] is actually built for a set of MCPs) lives behind
//! [`RuntimeFactory`], so this crate stays testable with a mock and the real turbomcp-backed
//! factory is a separate concern.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use liberado_common::{
    ApprovedGuard, BlockReason, CONSEQUENCE_GATE, Capability, CapabilitySet, Consequence, Delivery,
    Depth, DispatchAction, DispatchDecision, McpDescriptor, Outcome, Proposal, ProposalSigner,
    ProposedAction, Report, SignedProposal, ToolCall, WriteClass, WriteProvenance, mcp_of,
};
use liberado_executor::{
    Budget, ExecError, Executor, LoopProfile, RiskGatedToolRuntime, RuntimeFactory,
    RuntimeSetupError, SUBMIT_REPORT_TOOL, Task, ToolRuntime,
};
use liberado_notify::Notifier;
use liberado_provider::{Provider, ToolDef, ToolInvocation};
use liberado_session::TerminalKind;
use thiserror::Error;
use tokio::sync::Semaphore;
use tracing::Instrument;

/// Default `source` recorded in write provenance for orchestrated executions.
pub const DEFAULT_SOURCE: &str = "liberado-executor";

/// The one tool call that lands a [`Delivery::Vault`] report, as declared in `topology.toml`.
///
/// The orchestrator is kernel-layer and does not depend on `liberado-vault`, so it reaches the
/// vault the way everything else does — an MCP tool call through the [`RuntimeFactory`]. It must
/// therefore be *told* which tool and which argument names, rather than assuming TurboVault's
/// `write_note(path, content)`. `liberado-config-loader`'s validation refuses to boot on a sink
/// that names a missing, disabled, read-only, or non-writing tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSink {
    /// MCP that owns the write tool.
    pub mcp: String,
    /// Bare tool name (the fully-qualified `"<mcp>:<tool>"` form is built when invoking).
    pub tool: String,
    /// Argument carrying the destination path.
    pub path_arg: String,
    /// Argument carrying the report body.
    pub content_arg: String,
}

impl ReportSink {
    pub fn new(
        mcp: impl Into<String>,
        tool: impl Into<String>,
        path_arg: impl Into<String>,
        content_arg: impl Into<String>,
    ) -> Self {
        Self {
            mcp: mcp.into(),
            tool: tool.into(),
            path_arg: path_arg.into(),
            content_arg: content_arg.into(),
        }
    }

    /// The `"<mcp>:<tool>"` name the runtime dispatches on (see [`mcp_of`]).
    fn qualified_tool(&self) -> String {
        format!("{}:{}", self.mcp, self.tool)
    }
}

/// Exact [`Report::summary`] when [`Orchestrator::execute_approved`] refuses a past-deadline
/// proposal without running tools. Callers (daemon) must match this string exactly — do not use
/// substring checks against free-form executor summaries.
pub const EXPIRED_PROPOSAL_REFUSAL_SUMMARY: &str = "proposal expired — not executed";

/// Turn budget for an `ExecuteDirect` (kept tight — it is the "few steps clearly suffice" path).
pub const DIRECT_MAX_TURNS: u32 = 4;

/// Turn budget for a **read-only** subagent — research, review, summarisation.
///
/// Deliberately far above the general subagent default. Gathering work is turn-hungry in a way
/// that acting work is not: a live deep-research run spent all 8 of the default turns on ~28
/// searches and never reached its write-up. Nothing such a run touches can be left half-changed,
/// so the only cost of a long ceiling is tokens, and the wrap-up reserve
/// ([`liberado_executor::WRAP_UP_TURNS`]) guarantees the findings come back even at the ceiling.
///
/// Override per deployment with [`Orchestrator::with_research_budget`].
pub const RESEARCH_MAX_TURNS: u32 = 30;

/// The executor's system prompt for an `ExecuteDirect` tool loop. `pub` (like
/// `Dispatcher::DEFAULT_SYSTEM_PROMPT`) so `liberado-heuristics-tuner` can read it as the seed
/// baseline for executor-layer prompt tuning — adopting a winning candidate is still a manual
/// hand-edit of this const, never auto-merged (Decision 14).
pub const DIRECT_INSTRUCTIONS: &str = "\
You are Liberado's executor. Accomplish the goal using the available tools, taking as few steps as \
possible. When the goal is done, call `submit_report` with a concise, high-signal result. Do not \
ask the user anything; if you cannot proceed, submit a report explaining why.";

/// The subagent's system prompt preamble for a `DispatchSubagent` task (success criteria are
/// appended per-task by `subagent_instructions`). `pub` for the same reason as
/// `DIRECT_INSTRUCTIONS` above — a seed baseline for subagent-layer prompt tuning.
pub const SUBAGENT_PREAMBLE: &str = "\
You are a narrowly-scoped Liberado subagent. Use only the tools you have been given to accomplish \
the goal, then call `submit_report` with the result. Do not exceed your goal. Prefer the smallest \
tool sequence that answers the goal (e.g. list_tasks or search, then report) — avoid long \
scratchpad loops or re-reading the same notes when you already have enough to report.";

/// What an orchestrated decision resolved to.
///
/// **Not a status enum, and deliberately not merged with one** (V1, 2026-07-14). It is a sum type
/// *carrying payloads* — a `Report`, a set of clarifying questions, a `SignedProposal` — and the
/// payload is the point. "Succeeded/Failed" is a different question, answered by
/// [`terminal_summary`](Self::terminal_summary), which is the single conversion from this to a
/// session's `TerminalKind`. It is shared by the daemon and the dispatch pack precisely so the two
/// cannot drift on what a `Propose` disposition *means* when a session ends on one.
// Payload-carrying sum type by design (see module docs above) — variants hold real artifacts,
// not status tags. Boxing would only paper over the intentional shape for a clippy threshold.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Disposition {
    /// Work ran; here is the report for the main agent.
    Reported(Report),
    /// The dispatcher asked to clarify before acting — nothing was executed.
    Clarify {
        questions: Vec<String>,
        what_blocked: BlockReason,
    },
    /// A high-consequence action was downgraded to a proposal for human approval (Decision 11).
    /// The orchestrator only *builds* the artifact; the daemon (which owns the vault) writes it.
    /// Already signed (`SignedProposal`, not a bare `Proposal`) — a write helper that takes this
    /// type can't forget to sign, by construction.
    Propose(SignedProposal),
}

impl Disposition {
    /// How this disposition reads as the terminal state of a **background session** (S5′ step 5).
    ///
    /// Both unattended callers — the daemon reacting to a cron/webhook, and the face agent's
    /// `delegate` — need exactly this mapping, so it lives here rather than in each of them, where
    /// the two copies would eventually disagree about whether a proposal "succeeded".
    ///
    /// The honest reading matters more than a flattering one: a background session's status is the
    /// only thing a human sees at a glance, so a green tick on work that never ran would be worse
    /// than no session at all.
    ///
    /// * **Reported** ⇒ the executor's own outcome; only `Failed` is a failure. A partial success
    ///   says so in the summary rather than hiding behind a status.
    /// * **Clarify** ⇒ `Failed`. It needed a human and there was none — which is what a background
    ///   session *is*. The unanswered questions go into the summary so they are not simply lost.
    /// * **Propose** ⇒ `Succeeded`. Escalating a high-consequence action to a proposal *is* the
    ///   designed correct outcome (Decision 11) and leaves a durable artifact; the summary says
    ///   plainly that nothing has been executed yet.
    ///
    /// `TerminalKind` has no "awaiting review" variant and this does not invent one: a session left
    /// non-terminal would be coerced to `Failed` by the store's replay on the next boot, quietly
    /// turning every pending proposal into a failure.
    pub fn terminal_summary(&self) -> (TerminalKind, String) {
        match self {
            Disposition::Reported(report) => {
                let terminal = match report.outcome {
                    Outcome::Failed => TerminalKind::Failed,
                    _ => TerminalKind::Succeeded,
                };
                let summary = match report.outcome {
                    Outcome::PartiallySucceeded => {
                        format!("partially succeeded: {}", report.summary)
                    }
                    _ => report.summary.clone(),
                };
                (terminal, summary)
            }
            Disposition::Clarify { questions, .. } => (
                TerminalKind::Failed,
                format!(
                    "blocked — needed a human, and nobody was there to ask: {}",
                    questions.join(" / ")
                ),
            ),
            Disposition::Propose(proposal) => (
                TerminalKind::Succeeded,
                format!(
                    "escalated to a proposal for your approval (nothing executed yet): {}",
                    proposal.rationale
                ),
            ),
        }
    }

    /// Whether this run deferred the action to the human **and** already surfaced it out-of-band —
    /// the signal a chat surface uses to drop a redundant "you need to grant permission" reply
    /// (Gap 2). Only `Reported` can answer here: its flag is set by the runtime during execution.
    /// A `Propose`'s out-of-band notify is done by whoever *writes* the note (the dispatch pack /
    /// daemon) **after** this disposition is returned, so its notified-state is only known there —
    /// this returns `false` for it, and the writer ORs in its own send result.
    pub fn deferred_to_human(&self) -> bool {
        match self {
            Disposition::Reported(report) => report.deferred_to_human,
            Disposition::Clarify { .. } | Disposition::Propose(_) => false,
        }
    }
}

/// A single sub-goal to dispatch in parallel. Each is capability-narrowed to the MCPs its
/// sub-goal actually needs.
pub struct SubDispatch {
    /// The goal the subagent should accomplish.
    pub goal: String,
    /// The MCP servers this subagent is allowed to use.
    pub allowed_mcps: Vec<String>,
    /// Criteria the subagent should meet before reporting success.
    pub success_criteria: Vec<String>,
    /// Correlation id for provenance — ties every tool write back to this dispatch.
    pub correlation_id: String,
    /// Human-readable label for the merged report.
    pub label: String,
    /// Filesystem root the worker must operate inside, or `None` for an unconstrained worker.
    ///
    /// **Placement (C7):** this field is the kernel-side *seam*, not the isolation itself. The
    /// orchestrator never creates or manages the workspace — it forwards the root to
    /// [`RuntimeFactory::runtime_for_in`] unchanged, and the caller-supplied factory decides
    /// what an isolated workspace means. The concrete git-worktree primitive
    /// (`WorktreeWorkspace`) lives in `coder-sandbox` (pack).
    pub workspace_root: Option<PathBuf>,
}

/// Errors from orchestrating a decision. (Tool-level failures are *not* here — the executor feeds
/// those back to the model in-band; a `Failed` outcome still arrives as a [`Report`].)
#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("runtime setup failed: {0}")]
    Runtime(#[from] RuntimeSetupError),
    #[error(transparent)]
    Execution(#[from] ExecError),
}

/// Maps dispatcher decisions to executions. Holds the factory behind a boxed trait object so the
/// daemon can own an `Orchestrator` without becoming generic.
pub struct Orchestrator {
    provider: Arc<dyn Provider>,
    /// Provider for **delegated subagent** work — a second instance over the same model backend,
    /// tagged `AgentRole::Subagent` so subagent calls journal separately from the orchestrator's own
    /// direct execution (`ExecuteDirect`, which uses [`provider`](Self::provider)). Defaults to
    /// `provider` (see [`Self::new`]); override via [`Self::with_subagent_provider`].
    subagent_provider: Arc<dyn Provider>,
    factory: Box<dyn RuntimeFactory>,
    /// The ceiling `ExecuteDirect` scopes its runtime to (see `run`'s `ExecuteDirect` arm). The
    /// same `CapabilitySet` the caller's dispatch request was checked against — passing a wider
    /// one here would let a direct execution reach MCPs the guard pre-flight never considered.
    capabilities: CapabilitySet,
    /// `(mcp_name, consequence)` pairs for the runtime-level gate's consequence check (see `gate`).
    /// Fallback when [`live_catalog`](Self::with_live_catalog) is unset.
    consequence_catalog: Vec<(String, Consequence)>,
    /// MCP descriptors (zone declarations) for the runtime-level gate's zone-write-class check.
    zone_catalog: Vec<McpDescriptor>,
    /// Live capability catalog for hot-reload-safe gate lookups (preferred over the Vec snapshots).
    live_catalog: Option<Arc<liberado_common::CapabilityCatalog>>,
    /// `(zone, write_class)` pairs from `Policy.zones` for the same check.
    zone_write_classes: Vec<(String, WriteClass)>,
    /// Declarative risk waivers from `policy.toml`. Passed through to every `gate` runtime
    /// built by this orchestrator so the runtime magnitude guard sees the same waivers the
    /// dispatcher's pre-flight guard does. Empty = unchanged pre-feature behaviour.
    risk_waivers: liberado_common::RiskWaiverSet,
    /// Base directory for proposal files a runtime-level downgrade writes (see `gate`).
    proposals_dir: PathBuf,
    /// Signs proposals built by the `Propose` arm and this orchestrator's own runtime-level `gate`
    /// downgrades; also checked defensively in `execute_approved` (see that method's doc comment).
    signer: ProposalSigner,
    /// Which named dispatcher/executor pool (Decision 18 checkpoint #3) this orchestrator *is* —
    /// stamped onto every proposal this orchestrator builds (both the `Propose` arm and `gate`'s
    /// runtime-level downgrades), signed as part of `Proposal.pool`, so an approval later executes
    /// it via *this same* pool's authority, never a different (possibly broader) one. Every
    /// `Orchestrator` belongs to exactly one pool — the always-present `"default"` pool for
    /// anything that predates this, or a caller-chosen name for an additional pool.
    pool_name: String,
    source: String,
    direct_budget: Budget,
    subagent_budget: Budget,
    /// Budget for read-only ("research") subagent work — see [`RESEARCH_MAX_TURNS`].
    research_budget: Budget,
    /// Told about every proposal a runtime-level `gate` downgrade writes — optional, `None` by
    /// default. Best-effort: a notification failure never blocks the write it's reporting on.
    notifier: Option<Arc<dyn Notifier>>,
    /// Where a [`Delivery::Vault`] report is written. `None` → vault delivery is unavailable and
    /// every report is summarized by the main agent, exactly as before this existed.
    report_sink: Option<ReportSink>,
}

/// The 6 of [`Orchestrator::new`]'s 9 parameters that are the same for every pool a given daemon
/// configures (Decision 18 checkpoint #3) — everything except a pool's own [`RuntimeFactory`]
/// (registries aren't `Clone`/shareable across orchestrators), its `CapabilitySet` ceiling, and its
/// name. `crates/bootstrap/src/lib.rs`'s `configure_daemon` used to build these 6 values once via
/// `liberado_config::guard_context` and then re-clone all of them into `Orchestrator::new` at
/// every pool's call site (`docs/future-work/archive/hygiene-audit-2026-07-05.md`) — building one
/// `OrchestratorInfra` and calling [`for_pool`](Self::for_pool) per pool collapses that back down to
/// naming only what actually differs.
///
/// Also shared here: the subagent-tagged provider (defaults to the infra's own `provider`, set via
/// [`with_subagent_provider`](Self::with_subagent_provider)) — the second instance every pool uses
/// for delegated subagent work so it journals under `AgentRole::Subagent` rather than
/// `AgentRole::Orchestrator`.
pub struct OrchestratorInfra {
    provider: Arc<dyn Provider>,
    /// Tagged `AgentRole::Subagent` — the second provider handed to every pool built from this
    /// infra so delegated subagent work journals separately from direct execution. Defaults to
    /// `provider` (see [`Self::new`]); override via [`Self::with_subagent_provider`].
    subagent_provider: Arc<dyn Provider>,
    /// Shared live catalog — gate consequence/zone data is refreshed from here after MCP apply.
    live_catalog: Arc<liberado_common::CapabilityCatalog>,
    zone_write_classes: Vec<(String, WriteClass)>,
    /// Declarative risk waivers from `policy.toml`. Passed through to every pool so the
    /// runtime magnitude guard and the dispatcher's pre-flight guard see the same set.
    risk_waivers: liberado_common::RiskWaiverSet,
    proposals_dir: PathBuf,
    signer: ProposalSigner,
    /// Turn ceiling for read-only subagent work, applied to every pool built from this infra.
    research_max_turns: u32,
    /// Vault report sink shared by every pool built from this infra.
    report_sink: Option<ReportSink>,
}

impl OrchestratorInfra {
    pub fn new(
        provider: Arc<dyn Provider>,
        live_catalog: Arc<liberado_common::CapabilityCatalog>,
        zone_write_classes: Vec<(String, WriteClass)>,
        proposals_dir: PathBuf,
        signer: ProposalSigner,
    ) -> Self {
        Self {
            subagent_provider: provider.clone(),
            provider,
            live_catalog,
            zone_write_classes,
            // Default: empty. Operators opt in via `with_risk_waivers` once the
            // bootstrap wires the loaded `Policy` through.
            risk_waivers: liberado_common::RiskWaiverSet::empty(),
            proposals_dir,
            signer,
            research_max_turns: RESEARCH_MAX_TURNS,
            report_sink: None,
        }
    }

    /// Set the subagent-tagged provider every pool built from this infra uses for delegated
    /// subagent work (see [`Orchestrator::subagent_provider`]). Defaults to the infra's own
    /// `provider` — set it whenever subagent calls must journal under a distinct role.
    pub fn with_subagent_provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.subagent_provider = provider;
        self
    }

    /// Set the vault report sink for every pool built from this infra (see [`ReportSink`]).
    pub fn with_report_sink(mut self, sink: ReportSink) -> Self {
        self.report_sink = Some(sink);
        self
    }

    /// Set the read-only ("research") subagent turn ceiling for every pool built from this infra.
    ///
    /// Exists so the deployment can tune it from `topology.toml` without a rebuild — the same
    /// treatment the per-role model settings get.
    pub fn with_research_max_turns(mut self, max_turns: u32) -> Self {
        self.research_max_turns = max_turns;
        self
    }

    /// Set the risk-waiver set propagated to every pool's runtime magnitude guard.
    pub fn with_risk_waivers(mut self, waivers: liberado_common::RiskWaiverSet) -> Self {
        self.risk_waivers = waivers;
        self
    }

    /// Build the [`Orchestrator`] for one pool: only what's actually pool-specific — its
    /// [`RuntimeFactory`], its capability ceiling, and its name — combined with this shared infra.
    pub fn for_pool(
        &self,
        factory: impl RuntimeFactory + 'static,
        capabilities: CapabilitySet,
        pool_name: impl Into<String>,
    ) -> Orchestrator {
        Orchestrator {
            provider: self.provider.clone(),
            subagent_provider: self.subagent_provider.clone(),
            factory: Box::new(factory),
            capabilities,
            // Snapshots kept for API compatibility; live_catalog wins on every invoke.
            consequence_catalog: self.live_catalog.consequence_catalog(),
            zone_catalog: self.live_catalog.descriptors(),
            live_catalog: Some(self.live_catalog.clone()),
            zone_write_classes: self.zone_write_classes.clone(),
            risk_waivers: self.risk_waivers.clone(),
            proposals_dir: self.proposals_dir.clone(),
            signer: self.signer.clone(),
            pool_name: pool_name.into(),
            source: DEFAULT_SOURCE.to_string(),
            direct_budget: Budget::new(DIRECT_MAX_TURNS),
            subagent_budget: Budget::default(),
            research_budget: Budget::new(self.research_max_turns),
            notifier: None,
            report_sink: self.report_sink.clone(),
        }
    }
}

impl Orchestrator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn Provider>,
        factory: impl RuntimeFactory + 'static,
        capabilities: CapabilitySet,
        consequence_catalog: Vec<(String, Consequence)>,
        zone_catalog: Vec<McpDescriptor>,
        zone_write_classes: Vec<(String, WriteClass)>,
        proposals_dir: PathBuf,
        signer: ProposalSigner,
        pool_name: impl Into<String>,
    ) -> Self {
        Self {
            subagent_provider: provider.clone(),
            provider,
            factory: Box::new(factory),
            capabilities,
            consequence_catalog,
            zone_catalog,
            live_catalog: None,
            zone_write_classes,
            // Default empty for the legacy entry point; the bootstrap-driven infra path
            // wires the loaded `Policy` via `with_risk_waivers`.
            risk_waivers: liberado_common::RiskWaiverSet::empty(),
            proposals_dir,
            signer,
            pool_name: pool_name.into(),
            source: DEFAULT_SOURCE.to_string(),
            direct_budget: Budget::new(DIRECT_MAX_TURNS),
            subagent_budget: Budget::default(),
            research_budget: Budget::new(RESEARCH_MAX_TURNS),
            notifier: None,
            report_sink: None,
        }
    }

    /// Override the turn budget for read-only subagent work (default [`RESEARCH_MAX_TURNS`]).
    pub fn with_research_budget(mut self, budget: Budget) -> Self {
        self.research_budget = budget;
        self
    }

    /// Override the provider used for delegated subagent work. Tag it `AgentRole::Subagent` so the
    /// journal distinguishes subagent calls from the orchestrator's own direct execution
    /// (which stays on [`provider`](Self::provider)). Defaults to the orchestrator's own provider.
    pub fn with_subagent_provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.subagent_provider = provider;
        self
    }

    /// Attach the vault report sink (see [`ReportSink`]). Without one, a `Delivery::Vault` request
    /// falls back to `Summarize` rather than failing — an unconfigured deployment behaves exactly
    /// as it did before delivery existed.
    pub fn with_report_sink(mut self, sink: ReportSink) -> Self {
        self.report_sink = Some(sink);
        self
    }

    /// The turn budget for a declared [`Depth`], capped by this pool's configured ceiling.
    ///
    /// `Deep` is the research budget — far above the default, because gathering is turn-hungry in a
    /// way acting is not and a long ceiling costs only tokens. The cap is what makes depth safe to
    /// let any dispatch source raise: an orchestration agent can ask for room within an envelope the
    /// operator set, never past it.
    /// The budget a run actually gets: the depth's configured ceiling, with `max_turns` replaced
    /// when the caller supplied one. Zero is treated as absent, matching `GoalSpec::max_turns`,
    /// where 0 already means "pack default".
    fn effective_budget(&self, base: &Budget, max_turns: Option<u32>) -> Budget {
        match max_turns.filter(|t| *t > 0) {
            Some(turns) => base.clone().with_max_turns(turns),
            None => base.clone(),
        }
    }

    fn budget_for(&self, depth: Depth) -> &Budget {
        match depth {
            Depth::Deep => &self.research_budget,
            Depth::Normal => &self.subagent_budget,
            Depth::Shallow => &self.direct_budget,
        }
    }

    /// Apply the two delivery guards and return the sink this run actually gets.
    ///
    /// Both guards can only **downgrade** to [`Delivery::Summarize`], never upgrade — same shape as
    /// the dispatcher's post-model guards, and for the same reason: delivery is chosen by a model,
    /// so it has to be checkable afterwards rather than trusted.
    ///
    /// 1. **Only a read-only dispatch may bypass the main agent.** If the subagent could act on the
    ///    world, the main agent narrates the result — it is the only participant that can re-dispatch,
    ///    ask a follow-up, or explain a half-completed action. This is a property of the MCP grant,
    ///    not a judgement about the task.
    /// 2. **Only a clean success is filed.** A failed or partial run is exactly when the detail
    ///    needs to be *in the conversation*, where it can be reacted to. Filing a half-finished
    ///    write-up under a name that implies a finished document is its own small harm, and nothing
    ///    is lost by the fallback: the findings still reach the human, just narrated.
    ///
    /// Together these make a mis-routed delivery cheap. Wrongly filing a successful retrieval only
    /// means the human reads it unfiltered; wrongly filing a *failure* cannot happen.
    fn resolve_delivery(
        &self,
        requested: &Delivery,
        allowed_mcps: &[String],
        outcome: Outcome,
    ) -> (Delivery, Option<&'static str>) {
        // Everything except the outcome is knowable before the run — and has to be, because the
        // subagent must be *told* its report is the deliverable (see `delivery_target`).
        match self.delivery_target(requested, allowed_mcps) {
            Err(reason) => (Delivery::Summarize, reason),
            Ok(_) if outcome != Outcome::Succeeded => (
                Delivery::Summarize,
                Some("run did not cleanly succeed — the main agent needs the detail to react"),
            ),
            Ok(path) => (Delivery::Vault { path }, None),
        }
    }

    /// Which output contract a subagent gets, given where its report is going.
    ///
    /// Extracted so the *wiring* is testable, not just the wording. The directives are pure functions
    /// and easy to assert on, which made it possible to have thorough tests of text that was never
    /// appended to anything — removing the call site failed nothing.
    ///
    /// Three cases, and the empty one is deliberate:
    /// * **filed to the vault** — the summary is the artifact ([`delivery_directive`]);
    /// * **research relayed to the main agent** — the summary is the material ([`relay_directive`]);
    /// * **action with no delivery** — nothing. The work produced an artifact of its own, so a short
    ///   status genuinely is the right report and asking for more would put a wall of text in a chat
    ///   that wanted "done".
    fn output_contract(
        &self,
        delivery: &Delivery,
        allowed_mcps: &[String],
        research: bool,
    ) -> String {
        match self.delivery_target(delivery, allowed_mcps) {
            Ok(target) => delivery_directive(&target),
            Err(_) if research => relay_directive(),
            Err(_) => String::new(),
        }
    }

    /// Where a requested delivery would land, decided **without** the outcome.
    ///
    /// Split out from [`resolve_delivery`](Self::resolve_delivery) because the answer is needed at
    /// two different times. The outcome check can only run after execution — but the *subagent* has
    /// to be told before execution that its report will be filed verbatim, or it writes a status
    /// line and waits for a chance to author the document that never comes. That is not
    /// hypothetical: the first live run filed 231 bytes reading "I have all the research I need.
    /// Let me now write the comprehensive report directly to the vault." Every mechanism was
    /// correct and the artifact was the model's narration of a step we had removed its tools for.
    ///
    /// `Err(None)` means "nothing was requested" — not a downgrade, so nothing to log.
    fn delivery_target(
        &self,
        requested: &Delivery,
        allowed_mcps: &[String],
    ) -> Result<String, Option<&'static str>> {
        let path = match requested {
            Delivery::Summarize => return Err(None),
            Delivery::Vault { path } => path,
        };
        if self.report_sink.is_none() {
            return Err(Some("no report sink configured"));
        }
        if !self.delivery_consequence_ok(allowed_mcps) {
            return Err(Some(
                "dispatch can act outside the vault — the main agent narrates anything with \
                 irreversible or external effects",
            ));
        }
        let clean = vault_delivery_path(path).map_err(Some)?;
        match self.delivery_write_refusal(&clean) {
            Some(reason) => Err(Some(reason)),
            None => Ok(clean),
        }
    }

    /// May a dispatch over these MCPs bypass the main agent?
    ///
    /// Gates on **consequence**, deliberately not on [`is_read_only_dispatch`](Self::is_read_only_dispatch).
    /// Those were the same predicate until a live run showed why they must not be:
    ///
    /// "Research X and save the write-up to my vault" is the clearest case there is for direct
    /// delivery — and it is exactly the phrasing that makes a classifier reach for the vault MCP.
    /// The vault is `Reversible`, so the dispatch stopped being "read-only", so delivery refused,
    /// so the report went back through the main agent to be paraphrased. The feature switched
    /// itself off precisely when the human had asked for it most plainly.
    ///
    /// The question delivery actually cares about is narrower than "did this write anything". It is
    /// **"did something happen that the main agent needs to narrate?"** — a sent email, a booked
    /// appointment, a filed API call. Those are `Irreversible`/`External`, and for those the main
    /// agent is the only participant that can re-dispatch, ask a follow-up, or explain a partial
    /// action. A `Reversible` vault write is not that: it left nothing outside the system, and it
    /// is a `git revert` away — the same reasoning [`Consequence`] already documents for rating a
    /// vault write below an email.
    ///
    /// Budget and delivery therefore ask different questions of the same MCP list, and get
    /// different answers. `is_read_only_dispatch` still governs turns and salvage, because *there*
    /// the question really is "could this have left something half-written".
    ///
    /// An unknown MCP fails closed, same as the budget derivation: an unrecognised name is not
    /// waved through to bypass the main agent.
    fn delivery_consequence_ok(&self, allowed_mcps: &[String]) -> bool {
        if allowed_mcps.is_empty() {
            // Empty means "the full ceiling" — an unbounded scope we cannot rate. Fail closed.
            return false;
        }
        let catalog = match &self.live_catalog {
            Some(cat) => cat.consequence_catalog(),
            None => self.consequence_catalog.clone(),
        };
        allowed_mcps.iter().all(|mcp| {
            catalog
                .iter()
                .find(|(name, _)| name == mcp)
                .is_some_and(|(_, consequence)| *consequence < CONSEQUENCE_GATE)
        })
    }

    /// Why this orchestrator may **not** write a delivery to `path`, if it may not.
    ///
    /// `deliver_to_vault` deliberately does not run through [`gate`](Self::gate): a
    /// `RiskGatedToolRuntime` would turn a restricted zone into a *proposal*, and a delivery is
    /// supposed to be one silent write or nothing at all — nobody wants an approval request for
    /// filing a research note. But skipping the gate cannot mean skipping its *rules*, or this
    /// becomes an unguarded write path straight into the vault, which is precisely the F1 shape:
    /// a guard that is silently absent because a new code path grew around it.
    ///
    /// So the two checks the gate would have made are made here, statically, and a failure
    /// downgrades to `Summarize` rather than proposing:
    ///
    /// * the target zone must be directly agent-writable — an undeclared zone defaults to
    ///   `ProposalOnly` (`Policy::write_class`'s fail-safe), so an invented path is refused, not
    ///   filed;
    /// * this pool must actually hold `Write` on that zone. The orchestrator writes under its own
    ///   authority here, so it has to have the authority.
    fn delivery_write_refusal(&self, path: &str) -> Option<&'static str> {
        let zone = path.split('/').next().unwrap_or_default();
        let class = self
            .zone_write_classes
            .iter()
            .find(|(name, _)| name == zone)
            .map(|(_, class)| *class)
            .unwrap_or_default();
        if !class.allows_direct_agent_write() {
            return Some(
                "target zone is not directly agent-writable (undeclared zones default so)",
            );
        }
        if !self
            .capabilities
            .contains(&Capability::Write(liberado_common::Zone::vault(zone)))
        {
            return Some("no Write capability for the target zone");
        }
        None
    }

    /// Route a finished subagent `Report` to its requested sink, after the guards have had their
    /// say. The single entry point both dispatch paths (live and approved-proposal) call, so the
    /// two cannot drift on what delivery means.
    ///
    /// A downgrade is logged at `info` with its reason. That is not noise: delivery is a
    /// model-chosen route, and "the report went somewhere other than where it was routed" was
    /// invisible in the deep-research incident that motivated all of this — the report simply never
    /// appeared, and finding out why took a full log read of the container's life.
    async fn deliver(
        &self,
        report: &mut Report,
        requested: &Delivery,
        allowed_mcps: &[String],
        correlation_id: &str,
    ) -> Result<(), OrchestratorError> {
        let (effective, downgrade) = self.resolve_delivery(requested, allowed_mcps, report.outcome);
        if let Some(reason) = downgrade {
            tracing::info!(
                requested = requested.label(),
                effective = effective.label(),
                reason,
                "delivery downgraded"
            );
        }
        match effective {
            Delivery::Summarize => Ok(()),
            Delivery::Vault { path } => self.deliver_to_vault(report, &path, correlation_id).await,
        }
    }

    /// Perform a resolved [`Delivery::Vault`]: one deterministic tool call, no model in the loop,
    /// then rewrite the report into a **receipt** so the main agent has nothing to restate.
    ///
    /// Runs on its own runtime scoped to the sink MCP alone. That is deliberate — the subagent's
    /// runtime is read-only by construction (guard #1 above), and it stays that way. The write is
    /// the *orchestrator's*, made once, with this dispatch's provenance, which is also the honest
    /// account of what happened: a subagent that gathered, and a system that filed.
    async fn deliver_to_vault(
        &self,
        report: &mut Report,
        path: &str,
        correlation_id: &str,
    ) -> Result<(), OrchestratorError> {
        let Some(sink) = &self.report_sink else {
            return Ok(()); // unreachable: resolve_delivery downgrades without a sink.
        };

        // Check the report is a document before filing it as one.
        //
        // The subagent declares its own success, and on the delivery path nothing else looks: the
        // orchestrator writes whatever came back, verbatim, and the human finds out by opening the
        // note. A live run filed 231 bytes reading "I have all the research I need. Let me now write
        // the comprehensive report directly to the vault" — outcome Succeeded, path correct,
        // provenance correct, and the artifact was the model narrating an intention.
        //
        // `delivery_directive` now tells the subagent its report IS the document, which fixed that
        // case. This is the mechanical version of the same guarantee: a prompt holds while the model
        // complies, the prompt is unedited, and the provider does not drift. A length check holds
        // regardless. An LLM grading its own output would add nothing — that is the
        // "self-congratulation" the orchestration survey warns about — so this is deliberately a
        // dumb, deterministic assertion.
        if let Err(why) = looks_like_a_document(&report.summary) {
            tracing::warn!(
                path,
                bytes = report.summary.len(),
                why,
                "report failed delivery verification — falling back to the main agent"
            );
            report.outcome = Outcome::PartiallySucceeded;
            report.summary = format!(
                "NOTE: this was meant to be filed to `{path}` as a document, but it does not look \
                 like one ({why}), so it was not written. Relay what is below and say the write-up \
                 did not come through.\n\n{}",
                report.summary
            );
            return Ok(());
        }

        let body = vault_note_body(report, correlation_id, &self.source);
        let bytes = body.len();

        let provenance = WriteProvenance::agent(self.source.clone(), correlation_id);
        let runtime = self
            .factory
            .runtime_for(std::slice::from_ref(&sink.mcp), provenance)
            .await?;
        let inv = ToolInvocation::new(
            format!("deliver-{correlation_id}"),
            sink.qualified_tool(),
            serde_json::json!({
                sink.path_arg.as_str(): path,
                sink.content_arg.as_str(): body,
            }),
        );

        match runtime.invoke(&inv).await {
            Ok(_) => {
                tracing::info!(path, bytes, tool = %sink.qualified_tool(), "delivered report to vault");
                report.artifacts.push(path.to_string());
                report.summary = vault_receipt(path, bytes);
                Ok(())
            }
            Err(e) => {
                // Never hand back a receipt for a note that does not exist. Keep the full body (it
                // is the only copy) and say plainly that filing failed, so the main agent tells the
                // human instead of pointing them at an empty path.
                tracing::error!(path, tool = %sink.qualified_tool(), error = %e, "report delivery failed");
                report.outcome = Outcome::PartiallySucceeded;
                report.summary = format!(
                    "NOTE: could not write this report to `{path}` ({e}). The work below succeeded \
                     — it just is not filed, so relay it here.\n\n{}",
                    report.summary
                );
                Ok(())
            }
        }
    }

    /// Can this dispatch only *read*?
    ///
    /// True when every MCP the subagent is allowed to touch is declared `ReadOnly`. Such a run
    /// cannot leave anything half-written, which is what makes both the long research budget and
    /// the wrap-up reserve safe to grant.
    ///
    /// An **unknown** MCP name is treated as not-read-only. The name should always resolve, but if
    /// the catalog and the classifier ever disagree, the failure that costs nothing is a research
    /// run getting the ordinary budget — not an unrecognised writer getting 30 turns and being
    /// told its partial work is worth reporting.
    fn is_read_only_dispatch(&self, allowed_mcps: &[String]) -> bool {
        if allowed_mcps.is_empty() {
            return false;
        }
        let catalog = match &self.live_catalog {
            Some(cat) => cat.consequence_catalog(),
            None => self.consequence_catalog.clone(),
        };
        allowed_mcps.iter().all(|mcp| {
            catalog
                .iter()
                .find(|(name, _)| name == mcp)
                .is_some_and(|(_, consequence)| *consequence == Consequence::ReadOnly)
        })
    }

    /// Prefer live catalog lookups in the runtime gate (topology MCP hot-reload).
    pub fn with_live_catalog(mut self, catalog: Arc<liberado_common::CapabilityCatalog>) -> Self {
        self.live_catalog = Some(catalog);
        self
    }

    /// Override the provenance `source` recorded for executions (e.g. a per-deployment id).
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Attach a [`Notifier`] to tell about every proposal a runtime-level `gate` downgrade writes.
    /// Optional; an orchestrator with none attached just never sends anything, the same as today.
    pub fn with_notifier(mut self, notifier: Arc<dyn Notifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    /// Execute `decision`. `goal` is the goal an `ExecuteDirect` should accomplish (a
    /// `DispatchSubagent` carries its own restated goal). `trigger_correlation` is the id of the
    /// event that prompted this decision — the provenance correlation an `ExecuteDirect` adopts.
    ///
    /// `capabilities` is the **per-run** authority (a session grant, a profile, a chat turn). It is
    /// intersected **narrow-only** with this orchestrator's pool ceiling (`self.capabilities`) —
    /// Decision 4: authority never widens. A session profile that grants only `Read` is therefore
    /// genuinely refused a write its pool would have allowed (one-execution-engine plan E1).
    pub async fn run(
        &self,
        decision: DispatchDecision,
        goal: &str,
        trigger_correlation: &str,
        capabilities: &CapabilitySet,
    ) -> Result<Disposition, OrchestratorError> {
        self.run_with_turn_budget(decision, goal, trigger_correlation, capabilities, None)
            .await
    }

    /// [`run`](Self::run), with a caller-supplied turn cap replacing the one this run would
    /// otherwise inherit from its depth.
    ///
    /// Exists because the turn budgets are deployment constants — `DIRECT_MAX_TURNS` and the
    /// subagent default — while the work is not. A cron schedule doing N-item vault maintenance
    /// cannot ask for more, and it does not choose its own path either: the dispatcher picks
    /// `ExecuteDirect` (4 turns) or `DispatchSubagent` (8) from how the goal happens to be worded.
    /// Observed live: an inbox schedule routed to `ExecuteDirect` spent all four turns reading the
    /// vault and filed nothing.
    ///
    /// The override applies to whichever path is chosen, which is the point — it makes the routing
    /// decision stop determining whether the work can finish.
    ///
    /// `None`, or `Some(0)`, keeps the configured budget. Extra limits (wall-clock, tokens) are
    /// preserved: this raises the turn ceiling, it does not remove the others.
    pub async fn run_with_turn_budget(
        &self,
        decision: DispatchDecision,
        goal: &str,
        trigger_correlation: &str,
        capabilities: &CapabilitySet,
        max_turns: Option<u32>,
    ) -> Result<Disposition, OrchestratorError> {
        // Pool ceiling ∩ per-run grant. Order is deliberate: `narrow` filters *self* by *other*, so
        // nothing outside the pool can appear even if the caller passes a wider set.
        let mut effective = self.capabilities.narrow(capabilities);
        // Fold in any process-lifetime "Approve session" grant for this pool (see
        // `liberado_common::session_grants`). This is applied POST-narrow on purpose: a Session tap is
        // a *human-authorized widening* — the very capability the pool ceiling refused — so it must
        // survive the narrowing, exactly as "Approve everywhere" survives by widening the ceiling at
        // boot. Empty in the overwhelmingly common case (no session grants tapped). Downstream
        // write-class guards still apply, so this can't turn into a silent write to a restricted zone.
        for cap in liberado_common::session_grants::session_grant(&self.pool_name).capabilities {
            effective.grant(cap);
        }
        let action_label = decision.action.label();
        let span = tracing::info_span!(
            "orchestrate",
            action = action_label,
            source = %self.source,
            trigger = trigger_correlation,
            confidence = decision.confidence,
            disposition = tracing::field::Empty,
        );
        async {
            match decision.action {
                DispatchAction::Clarify {
                    questions,
                    what_blocked,
                } => {
                    tracing::Span::current().record("disposition", "clarify");
                    tracing::info!(?what_blocked, "dispatch resulted in clarify");
                    Ok(Disposition::Clarify {
                        questions,
                        what_blocked,
                    })
                }

                DispatchAction::Propose {
                    proposed_action,
                    rationale,
                } => {
                    // One proposal per trigger (v1): id == correlation == the triggering event, so
                    // the artifact is idempotent in the trigger and reuses the trigger's
                    // correlation when later executed. No vault write here — the daemon persists it.
                    let mut proposal = Proposal::pending(
                        trigger_correlation,
                        trigger_correlation,
                        self.source.clone(),
                        proposed_action,
                        rationale,
                    );
                    proposal.pool = Some(self.pool_name.clone());
                    let proposal = self.signer.sign(proposal);
                    tracing::Span::current().record("disposition", "proposed");
                    tracing::info!(proposal_id = %proposal.id, "dispatch resulted in a proposal");
                    Ok(Disposition::Propose(proposal))
                }

                DispatchAction::ExecuteDirect {
                    seed_calls,
                    relevant_mcps,
                    delivery,
                } => {
                    // The direct path has no `Depth` to select a budget from — it is always the
                    // shallow one — so the override is applied here rather than in `budget_for`.
                    let direct_budget = self.effective_budget(&self.direct_budget, max_turns);
                    // Scope to exactly the MCPs `effective` grants — an empty allow-list means
                    // "every registered MCP" to `RuntimeFactory`/`ScopedRuntime` (the wrong sense
                    // here, same reason `ChatSessions` special-cases it for its own scoping), which
                    // would let an adaptive (non-seed) tool call reach any registered MCP regardless
                    // of what the guard pre-flight actually checked the goal against.
                    let granted: Vec<String> = effective.granted_mcps();
                    // Further narrow within that ceiling when the classifier named which MCPs are
                    // actually relevant (token-efficiency — see `DispatchTuning::narrow_direct_tools`).
                    // Never widens: only MCPs already in `granted` survive the intersection, so a
                    // hallucinated `relevant_mcps` entry (already guard-checked, but belt and
                    // suspenders here too) can't grant more than the ceiling allows.
                    let allowed_mcps: Vec<String> = if relevant_mcps.is_empty() {
                        granted
                    } else {
                        granted
                            .into_iter()
                            .filter(|name| relevant_mcps.contains(name))
                            .collect()
                    };
                    // Same `research` derivation as the DispatchSubagent arm: whether every MCP
                    // this dispatch can reach is read-only. Used to decide whether the output
                    // contract asks for a relayable summary (chat delegate) or nothing (action).
                    let research = self.delivery_consequence_ok(&allowed_mcps);
                    let mut instructions = DIRECT_INSTRUCTIONS.to_string();
                    instructions.push_str(&self.output_contract(
                        &delivery,
                        &allowed_mcps,
                        research,
                    ));
                    tracing::info!(
                        seed_count = seed_calls.len(),
                        allowed_mcps = allowed_mcps.len(),
                        research,
                        delivery = delivery.label(),
                        "building execute-direct task"
                    );
                    let task = Task::new(instructions, goal).with_seed(seed_calls);
                    let mut report = if allowed_mcps.is_empty() {
                        self.execute(&direct_budget, &NoMcpRuntime, task).await?
                    } else {
                        let provenance =
                            WriteProvenance::agent(self.source.clone(), trigger_correlation);
                        let runtime = self.factory.runtime_for(&allowed_mcps, provenance).await?;
                        let (runtime, deferral) =
                            self.gate(runtime, effective.clone(), goal, trigger_correlation);
                        Self::instrument_catalog(&allowed_mcps, &*runtime);
                        let mut report = self.execute(&direct_budget, &*runtime, task).await?;
                        // If the gate deferred a call to the human out-of-band mid-run, mark it so a
                        // chat surface can drop the redundant reply (Gap 2).
                        report.deferred_to_human = deferred_flag_of(&deferral);
                        report
                    };
                    self.deliver(&mut report, &delivery, &allowed_mcps, trigger_correlation)
                        .await?;
                    tracing::Span::current().record("disposition", "reported");
                    tracing::info!(outcome = ?report.outcome, "execute-direct completed");
                    Ok(Disposition::Reported(report))
                }

                DispatchAction::DispatchSubagent {
                    goal: subgoal,
                    capabilities: decision_caps,
                    allowed_mcps,
                    success_criteria,
                    correlation_id,
                    delivery,
                    depth,
                    ..
                } => {
                    let provenance = WriteProvenance::agent(self.source.clone(), &correlation_id);
                    let runtime = self.factory.runtime_for(&allowed_mcps, provenance).await?;
                    // Decision 4: authority only shrinks. Classifier never emits `capabilities`
                    // (empty default); derive the gate from effective ∩ allowed_mcps so the risk
                    // gate matches the scoped tool catalog — not an empty set that blocks every MCP.
                    let gate_capabilities =
                        subagent_gate_capabilities(&effective, &decision_caps, &allowed_mcps);
                    let (runtime, deferral) = self.gate(
                        runtime,
                        gate_capabilities,
                        subgoal.as_str(),
                        correlation_id.as_str(),
                    );
                    Self::instrument_catalog(&allowed_mcps, &*runtime);
                    // Research = a subagent that can only read. Derived rather than declared:
                    // the classifier has no task-type field, but "every MCP it may touch is
                    // read_only" is exactly the property that matters here — such a run cannot
                    // leave anything half-written, so returning what it gathered is always safe,
                    // and gathering is the work that benefits from a long budget.
                    // Depth decides the budget; consequence decides salvage. These used to be one
                    // predicate (`is_read_only_dispatch`) and the conflation is what made a
                    // deep-research goal that merely *mentioned* the vault run on 8 turns and fail.
                    let budget = self.effective_budget(self.budget_for(depth), max_turns);
                    let budget = &budget;
                    // Salvageable = nothing irreversible could have happened, so returning partial
                    // findings is safe and honest. That is a consequence question, not a depth one —
                    // and deliberately still inferred: it is a safety property, not a preference,
                    // and so not a model's to declare.
                    let research = self.delivery_consequence_ok(&allowed_mcps);
                    tracing::debug!(
                        subagents = allowed_mcps.len(),
                        criteria = success_criteria.len(),
                        research,
                        max_turns = budget.max_turns,
                        "building subagent task"
                    );
                    // Search is the one place where varied arguments are the *work*: two
                    // different queries read as near-duplicates by a bag-of-words comparison, and
                    // a live research run was stopped three times for exactly that. Research
                    // therefore judges repeats byte-exactly — re-running the same query is still
                    // thrash and still trips the guard.
                    // Decided before the run, deliberately: a subagent whose report will be filed
                    // verbatim has to be told so up front, or it writes a status line and waits to
                    // author the document with a tool it was never given (see `delivery_directive`).
                    let mut instructions = SUBAGENT_PREAMBLE.to_string();
                    instructions.push_str(&self.output_contract(
                        &delivery,
                        &allowed_mcps,
                        research,
                    ));
                    if !success_criteria.is_empty() {
                        instructions.push_str(&format!(
                            "\n\nYou are done when:\n{}",
                            format_success_criteria(&success_criteria)
                        ));
                    }
                    let task = Task::new(instructions, subgoal)
                        .salvageable(research)
                        // Exact matching for deep work: varied search queries read as
                        // near-duplicates to a bag-of-words comparison, and a live research run was
                        // stopped three times for exactly that.
                        .loop_profile(if depth == Depth::Deep {
                            LoopProfile::exact()
                        } else {
                            LoopProfile::semantic()
                        });
                    // At info, not debug: with the budget as the only other signal, a research
                    // misclassification was previously invisible without raising the log level.
                    tracing::info!(
                        depth = depth.label(),
                        salvageable = research,
                        max_turns = budget.max_turns,
                        mcps = allowed_mcps.len(),
                        delivery = delivery.label(),
                        "dispatching subagent"
                    );
                    let mut report =
                        Self::execute_with(self.subagent_provider.clone(), budget, &*runtime, task)
                            .await?;
                    // Out-of-band deferral mid-run → mark it so a chat surface drops the redundant
                    // reply (Gap 2). This is the primary path for a delegated subagent's permission
                    // request bubbling up through `delegate`.
                    report.deferred_to_human = deferred_flag_of(&deferral);
                    self.deliver(&mut report, &delivery, &allowed_mcps, &correlation_id)
                        .await?;
                    tracing::Span::current().record("disposition", "reported");
                    tracing::info!(outcome = ?report.outcome, "subagent dispatch completed");
                    Ok(Disposition::Reported(report))
                }
            }
        }
        .instrument(span)
        .await
    }

    /// Execute an APPROVED proposal's action — approval is the authorization, so this bypasses the
    /// dispatcher/guards entirely (re-dispatching would just re-trigger the consequence guard and
    /// re-propose). What "bypasses the guards" means differs by variant: `ToolCalls` runs exactly
    /// the approved calls with no further gating (the calls themselves were what a human reviewed
    /// and approved); `Subagent` runs the approved goal/scoping through the same runtime-gated
    /// execution a live `DispatchSubagent` gets, because what was approved is the *goal and MCP
    /// scope*, not specific calls — the subagent still decides its own adaptive calls, so those
    /// still need the same per-call safety net (see `gate`'s doc comment).
    ///
    /// Checks the proposal's integrity signature before doing anything else — not a re-classification
    /// (approval still bypasses the risk guards, unchanged), but an authenticity check that this is
    /// actually the same proposal that was proposed, not a tampered or wholesale-forged one. The
    /// primary check lives in the daemon's `handle_proposal_change` (which must reject *before*
    /// calling this, so a failure is never marked done); this is defense in depth for any other
    /// caller reaching this method.
    pub async fn execute_approved(&self, proposal: &Proposal) -> Result<Report, OrchestratorError> {
        let span = tracing::info_span!(
            "execute_approved",
            proposal_id = %proposal.id,
            correlation = %proposal.correlation_id,
            source = %self.source,
        );
        async {
            if !self.signer.verify(proposal) {
                tracing::warn!(
                    proposal_id = %proposal.id,
                    "approved proposal failed integrity verification — refusing to execute"
                );
                return Ok(Report {
                    outcome: Outcome::Failed,
                    summary: "proposal failed integrity verification — not executed".into(),
                    artifacts: Vec::new(),
                    new_high_signal_facts: Vec::new(),
                    deferred_to_human: false,
                    follow_up: None,
                    repeat_calls: 0,
                });
            }

            // Defense in depth: daemon/Telegram gate wall-clock expiry first; refuse here so any
            // alternate caller cannot run a past-deadline approved proposal.
            if proposal.is_expired_at(chrono::Utc::now()) {
                tracing::warn!(
                    proposal_id = %proposal.id,
                    "approved proposal is past expires — refusing to execute"
                );
                return Ok(Report {
                    outcome: Outcome::Failed,
                    summary: EXPIRED_PROPOSAL_REFUSAL_SUMMARY.into(),
                    artifacts: Vec::new(),
                    new_high_signal_facts: Vec::new(),
                    deferred_to_human: false,
                    follow_up: None,
                    repeat_calls: 0,
                });
            }

            // Defense in depth (Decision 18 checkpoint #3): the *caller* (`Daemon::handle_proposal_change`)
            // is responsible for routing an approved proposal to the pool's orchestrator it was
            // proposed under, so a restricted pool's proposal never executes with a different
            // (possibly broader) pool's authority. Checked again here in case that routing is ever
            // wrong — never trust a single enforcement point for an authority boundary.
            let proposal_pool = proposal.pool.as_deref().unwrap_or(liberado_common::DEFAULT_POOL);
            if proposal_pool != self.pool_name {
                tracing::warn!(
                    proposal_id = %proposal.id,
                    proposal_pool,
                    orchestrator_pool = %self.pool_name,
                    "approved proposal's pool does not match this orchestrator — refusing to execute"
                );
                return Ok(Report {
                    outcome: Outcome::Failed,
                    summary: "proposal's pool does not match the executing orchestrator — not executed"
                        .into(),
                    artifacts: Vec::new(),
                    new_high_signal_facts: Vec::new(),
                    deferred_to_human: false,
                    follow_up: None,
                    repeat_calls: 0,
                });
            }

            match &proposal.proposed_action {
                ProposedAction::ToolCalls(calls) => {
                    self.execute_approved_tool_calls(proposal, calls).await
                }
                ProposedAction::Subagent {
                    goal,
                    capabilities,
                    allowed_mcps,
                    success_criteria,
                } => {
                    self.execute_approved_subagent(
                        proposal,
                        goal,
                        capabilities,
                        allowed_mcps,
                        success_criteria,
                    )
                    .await
                }
                ProposedAction::AdaptiveGoal {
                    goal,
                    capabilities,
                    relevant_mcps,
                    delivery,
                    approved_guard,
                } => {
                    self.execute_approved_adaptive_goal(
                        proposal,
                        goal,
                        capabilities,
                        relevant_mcps,
                        delivery,
                        *approved_guard,
                    )
                    .await
                }
                other => {
                    // VaultWrite/External/Other aren't produced by v1 emit; refuse defensively
                    // rather than error so the daemon can mark the proposal done and not retry
                    // forever.
                    tracing::warn!(
                        action = ?other,
                        "approved proposal action is not executable in v1"
                    );
                    Ok(Report {
                        outcome: Outcome::Failed,
                        summary: "proposed action type is not executable in v1".into(),
                        artifacts: Vec::new(),
                        new_high_signal_facts: Vec::new(),
                        deferred_to_human: false,
                        follow_up: None,
                        repeat_calls: 0,
                    })
                }
            }
        }
        .instrument(span)
        .await
    }

    /// `execute_approved`'s `ToolCalls` arm: run exactly the approved calls, in order, via a
    /// runtime scoped to the MCPs they touch, with the proposal's correlation id as provenance.
    async fn execute_approved_tool_calls(
        &self,
        proposal: &Proposal,
        calls: &[ToolCall],
    ) -> Result<Report, OrchestratorError> {
        // Scope the runtime to exactly the MCPs the approved calls touch (deduplicated, order
        // preserved). A runtime_for failure is an infra error and propagates.
        let mut allowed_mcps: Vec<String> = Vec::new();
        for call in calls {
            let mcp = mcp_of(&call.tool).to_string();
            if !allowed_mcps.contains(&mcp) {
                allowed_mcps.push(mcp);
            }
        }
        let provenance = WriteProvenance::agent(self.source.clone(), &proposal.correlation_id);
        let runtime = self.factory.runtime_for(&allowed_mcps, provenance).await?;

        // Run every approved call in order. Tool-level errors do NOT abort — they're folded into
        // the outcome (mirrors how the executor surfaces tool failures in-band).
        let mut ok = 0usize;
        let mut failed = 0usize;
        for (i, call) in calls.iter().enumerate() {
            let inv = ToolInvocation::new(format!("approved-{i}"), &call.tool, call.args.clone());
            match runtime.invoke(&inv).await {
                Ok(_) => ok += 1,
                Err(e) => {
                    tracing::warn!(tool = %call.tool, error = %e, "approved call failed");
                    failed += 1;
                }
            }
        }

        let outcome = if failed == 0 {
            Outcome::Succeeded
        } else if ok == 0 {
            Outcome::Failed
        } else {
            Outcome::PartiallySucceeded
        };
        tracing::info!(?outcome, ok, failed, "executed approved proposal");
        Ok(Report {
            outcome,
            summary: format!(
                "Executed approved proposal {} ({} call(s))",
                proposal.id,
                calls.len()
            ),
            artifacts: Vec::new(),
            new_high_signal_facts: Vec::new(),
            deferred_to_human: false,
            follow_up: None,
            repeat_calls: 0,
        })
    }

    /// Execute a signed empty-seed direct goal without sending it back through classification.
    /// The exact approved guard is skipped once for this run; capability, target resolution, and
    /// every other runtime guard remain active for each adaptive call.
    async fn execute_approved_adaptive_goal(
        &self,
        proposal: &Proposal,
        goal: &str,
        capabilities: &CapabilitySet,
        relevant_mcps: &[String],
        delivery: &Delivery,
        approved_guard: ApprovedGuard,
    ) -> Result<Report, OrchestratorError> {
        let effective = self.capabilities.narrow(capabilities);
        let granted = effective.granted_mcps();
        let allowed_mcps: Vec<String> = if relevant_mcps.is_empty() {
            granted
        } else {
            granted
                .into_iter()
                .filter(|name| relevant_mcps.contains(name))
                .collect()
        };
        let research = self.delivery_consequence_ok(&allowed_mcps);
        let mut instructions = DIRECT_INSTRUCTIONS.to_string();
        instructions.push_str(&self.output_contract(delivery, &allowed_mcps, research));
        let task = Task::new(instructions, goal);

        tracing::info!(
            proposal_id = %proposal.id,
            ?approved_guard,
            max_turns = self.direct_budget.max_turns,
            mcps = allowed_mcps.len(),
            "executing approved adaptive goal"
        );

        let mut report = if allowed_mcps.is_empty() {
            self.execute(&self.direct_budget, &NoMcpRuntime, task)
                .await?
        } else {
            let provenance =
                WriteProvenance::agent(self.source.clone(), proposal.correlation_id.as_str());
            let runtime = self.factory.runtime_for(&allowed_mcps, provenance).await?;
            let (runtime, deferral) = self.gate_with_approved_guard(
                runtime,
                effective,
                goal,
                proposal.correlation_id.as_str(),
                Some(approved_guard),
            );
            Self::instrument_catalog(&allowed_mcps, &*runtime);
            let mut report = self.execute(&self.direct_budget, &*runtime, task).await?;
            report.deferred_to_human = deferred_flag_of(&deferral);
            report
        };
        self.deliver(
            &mut report,
            delivery,
            &allowed_mcps,
            proposal.correlation_id.as_str(),
        )
        .await?;
        Ok(report)
    }

    /// `execute_approved`'s `Subagent` arm: dispatch the approved goal to a subagent scoped to
    /// `allowed_mcps`/`capabilities` (narrowed by the orchestrator's own ceiling, same
    /// belt-and-suspenders narrowing `run`'s `DispatchSubagent` arm does), with the proposal's
    /// correlation id as provenance. Runtime-gated (see `execute_approved`'s doc comment for why
    /// this variant, unlike `ToolCalls`, still needs it).
    async fn execute_approved_subagent(
        &self,
        proposal: &Proposal,
        goal: &str,
        capabilities: &CapabilitySet,
        allowed_mcps: &[String],
        success_criteria: &[String],
    ) -> Result<Report, OrchestratorError> {
        let provenance = WriteProvenance::agent(self.source.clone(), &proposal.correlation_id);
        let runtime = self.factory.runtime_for(allowed_mcps, provenance).await?;
        let gate_capabilities =
            subagent_gate_capabilities(&self.capabilities, capabilities, allowed_mcps);
        let (runtime, deferral) = self.gate(
            runtime,
            gate_capabilities,
            goal,
            proposal.correlation_id.as_str(),
        );
        Self::instrument_catalog(allowed_mcps, &*runtime);
        // Same derivation as the live dispatch arm: an approved proposal that can only read is
        // still research, and gets the same budget and the same right to file partial findings.
        let research = self.is_read_only_dispatch(allowed_mcps);
        let budget = if research {
            &self.research_budget
        } else {
            &self.subagent_budget
        };
        let task = Task::new(subagent_instructions(success_criteria), goal)
            .salvageable(research)
            .loop_profile(if research {
                LoopProfile::exact()
            } else {
                LoopProfile::semantic()
            });
        tracing::info!(
            research,
            max_turns = budget.max_turns,
            "executing approved subagent proposal"
        );
        let mut report =
            Self::execute_with(self.subagent_provider.clone(), budget, &*runtime, task).await?;
        report.deferred_to_human = deferred_flag_of(&deferral);
        // No `deliver` call here, and `ProposedAction::Subagent` carries no `delivery` — on purpose.
        // A subagent only becomes a proposal by tripping the consequence or zone-write guard, and
        // neither can fire on a dispatch whose every MCP is `ReadOnly`. So an approved subagent
        // proposal is never read-only, and `resolve_delivery` would downgrade it to `Summarize`
        // every time. Threading the field through the proposal type would be surface that provably
        // cannot change an outcome. If proposals ever gain a route that admits a read-only subagent,
        // this is the line that has to change with it.
        tracing::info!(outcome = ?report.outcome, research, "executed approved subagent proposal");
        Ok(report)
    }

    /// Run multiple subagent dispatches in parallel, each capability-narrowed to the MCPs
    /// its sub-goal actually needs. Results are collected into a single merged Report.
    /// Bounded by `max_concurrent` (from `tuning.dispatch.max_concurrent_subagents`).
    ///
    /// When a [`SubDispatch`] carries a [`workspace_root`](SubDispatch::workspace_root), the
    /// runtime factory is asked for a runtime scoped to that root via
    /// [`RuntimeFactory::runtime_for_in`]. The caller creates and cleans up those workspaces.
    /// Workers never fan out: a worker's runtime exposes no fan-out tool.
    pub async fn dispatch_parallel(
        &self,
        sub_dispatches: Vec<SubDispatch>,
        max_concurrent: usize,
    ) -> Result<Report, OrchestratorError> {
        let semaphore = Arc::new(Semaphore::new(max_concurrent.max(1)));
        let mut handles = Vec::with_capacity(sub_dispatches.len());
        // One deferral flag per sub-dispatch; OR'd into the merged report so a chat surface still
        // suppresses the redundant reply if *any* parallel subagent deferred out-of-band (Gap 2).
        let mut deferrals: Vec<Arc<AtomicBool>> = Vec::with_capacity(sub_dispatches.len());

        // `tokio::spawn` does not inherit task-locals, so each subagent would otherwise record its
        // inference under `correlation="-"` and detach from the parent turn in the latency journal.
        // Capture the parent correlation now and re-scope it inside each spawned worker.
        let correlation = liberado_provider::latency::current_correlation();

        for sub in sub_dispatches {
            // `acquire_owned` only errs if the semaphore was `.close()`d — this one is freshly
            // created above, local to this call, and never closed, so this cannot actually fail;
            // `.expect()` over `.unwrap()` so a future change that violates that assumption panics
            // with an explanation instead of a bare unwrap trace.
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore is local to this call and never closed");
            let provenance = WriteProvenance::agent(self.source.clone(), &sub.correlation_id);
            let runtime = self
                .factory
                .runtime_for_in(&sub.allowed_mcps, provenance, sub.workspace_root.clone())
                .await?;
            // No explicit CapabilitySet on `SubDispatch` — derive from allowed_mcps against the
            // ceiling (same rules as `DispatchSubagent` with empty decision capabilities).
            let gate_capabilities = subagent_gate_capabilities(
                &self.capabilities,
                &CapabilitySet::empty(),
                &sub.allowed_mcps,
            );
            let (runtime, deferral) = self.gate(
                runtime,
                gate_capabilities,
                sub.goal.as_str(),
                sub.correlation_id.as_str(),
            );
            Self::instrument_catalog(&sub.allowed_mcps, &*runtime);
            deferrals.push(deferral);
            let task = Task::new(subagent_instructions(&sub.success_criteria), sub.goal);
            let budget = self.subagent_budget.clone();
            let provider = self.subagent_provider.clone();
            let label = sub.label.clone();

            let correlation = correlation.clone();
            let handle = tokio::spawn(liberado_provider::latency::with_correlation(
                correlation,
                async move {
                    let result = Self::execute_with(provider, &budget, &*runtime, task).await;
                    drop(permit);
                    (label, result)
                },
            ));
            handles.push(handle);
        }

        // Collect results
        let mut summaries = Vec::new();
        let mut all_artifacts = Vec::new();
        let mut all_facts = Vec::new();
        let mut overall = Outcome::Succeeded;

        for handle in handles {
            match handle.await {
                Ok((label, Ok(report))) => {
                    summaries.push(format!("[{}] {}", label, report.summary));
                    all_artifacts.extend(report.artifacts);
                    all_facts.extend(report.new_high_signal_facts);
                    if report.outcome == Outcome::Failed
                        || report.outcome == Outcome::PartiallySucceeded
                    {
                        overall = Outcome::PartiallySucceeded;
                    }
                }
                Ok((label, Err(e))) => {
                    summaries.push(format!("[{}] failed: {e}", label));
                    overall = Outcome::PartiallySucceeded;
                }
                Err(e) => {
                    summaries.push(format!("[join error]: {e}"));
                    overall = Outcome::PartiallySucceeded;
                }
            }
        }

        Ok(Report {
            outcome: overall,
            summary: summaries.join("\n"),
            artifacts: all_artifacts,
            new_high_signal_facts: all_facts,
            follow_up: None,
            deferred_to_human: deferrals.iter().any(deferred_flag_of),
            repeat_calls: 0,
        })
    }

    async fn execute(
        &self,
        budget: &Budget,
        runtime: &dyn ToolRuntime,
        task: Task,
    ) -> Result<Report, ExecError> {
        Self::execute_with(self.provider.clone(), budget, runtime, task).await
    }

    /// The one canonical way an `Executor` gets built and run in this crate — takes an owned
    /// `provider` (not `&self`) specifically so [`dispatch_parallel`](Self::dispatch_parallel)'s
    /// spawned tasks can call it too: they run past this call's own borrow of `self` (moved into a
    /// `tokio::spawn`ed future), so they need an owned clone of the provider, not a method that
    /// borrows `&self`. Before this, `dispatch_parallel` built its own `Executor::new(...)` inline,
    /// duplicating exactly this line rather than sharing it (`docs/future-work/archive/hygiene-audit-2026-07-05.md`).
    async fn execute_with(
        provider: Arc<dyn Provider>,
        budget: &Budget,
        runtime: &dyn ToolRuntime,
        task: Task,
    ) -> Result<Report, ExecError> {
        Executor::new(provider, budget.clone())
            .execute(runtime, task)
            .await
    }

    fn instrument_catalog(offered: &[String], runtime: &dyn ToolRuntime) {
        let catalog = runtime.catalog();
        let (_, from_catalog, _) = catalog_measurements(offered, &catalog);
        match serde_json::to_string(&catalog) {
            Ok(schema) => {
                // chars/4 token proxy with a 1.3x safety factor (same
                // convention as `crates/main-agent/src/compaction.rs`).
                let schema_est_tokens = ((schema.len() as f64) / 4.0 * 1.3).ceil() as u64;
                tracing::info!(
                    mcp_offered = offered.len(),
                    mcp_from_catalog = from_catalog,
                    schema_bytes = schema.len(),
                    schema_est_tokens,
                    "tool catalog prepared"
                );
            }
            Err(error) => tracing::warn!(%error, "tool catalog serialization failed"),
        }
    }

    /// Wrap a connected runtime in the same runtime-level safety net chat's own tool loop already
    /// uses (`RiskGatedToolRuntime`), so the executor's *adaptive* (non-seed) tool calls get the
    /// same capability/consequence/magnitude checking the dispatcher's pre-flight guard only ever
    /// applied to the decision's seed call. Not used by `execute_approved`'s `ToolCalls` arm —
    /// approval is already the authorization for those *specific calls*; re-gating them would
    /// re-downgrade an approved call into a new proposal. `execute_approved`'s `Subagent` arm DOES
    /// use it, though: what a human approved there is a goal + MCP scope, not fixed calls, so the
    /// subagent's own adaptive calls during execution still need this same per-call safety net.
    ///
    /// Returns the gated runtime **and** the shared flag it raises when it defers a call to the
    /// human out-of-band, so the caller can read it back after the run and stamp it onto the
    /// `Report` (`Report::deferred_to_human`) — the signal a chat surface uses to drop a redundant
    /// "you need to grant permission" reply (Gap 2). See [`deferred_flag_of`] for the caller side.
    fn gate(
        &self,
        runtime: Box<dyn ToolRuntime>,
        capabilities: CapabilitySet,
        goal_context: impl Into<String>,
        correlation_base: impl Into<String>,
    ) -> (Arc<dyn ToolRuntime>, Arc<AtomicBool>) {
        self.gate_with_approved_guard(runtime, capabilities, goal_context, correlation_base, None)
    }

    fn gate_with_approved_guard(
        &self,
        runtime: Box<dyn ToolRuntime>,
        capabilities: CapabilitySet,
        goal_context: impl Into<String>,
        correlation_base: impl Into<String>,
        approved_guard: Option<ApprovedGuard>,
    ) -> (Arc<dyn ToolRuntime>, Arc<AtomicBool>) {
        let deferral_flag = Arc::new(AtomicBool::new(false));
        // Prefer live catalog so peers added via hot-reload still get consequence/zone gates.
        let (consequences, zones) = if let Some(cat) = &self.live_catalog {
            (cat.consequence_catalog(), cat.descriptors())
        } else {
            (self.consequence_catalog.clone(), self.zone_catalog.clone())
        };
        let mut gated = RiskGatedToolRuntime::new(
            Arc::from(runtime),
            capabilities,
            consequences,
            zones,
            self.zone_write_classes.clone(),
            self.proposals_dir.clone(),
            goal_context.into(),
            correlation_base.into(),
            self.signer.clone(),
            self.pool_name.clone(),
            self.risk_waivers.clone(),
        )
        .with_deferral_flag(deferral_flag.clone());
        if let Some(guard) = approved_guard {
            gated = gated.with_approved_guard(guard);
        }
        if let Some(cat) = &self.live_catalog {
            gated = gated.with_live_catalog(cat.clone());
        }
        if let Some(notifier) = &self.notifier {
            gated = gated.with_notifier(notifier.clone());
        }
        (Arc::new(gated), deferral_flag)
    }
}

/// Read a gate's deferral flag as a boolean — `true` iff the gated runtime raised a proposal /
/// permission-request during the run and surfaced it out-of-band (see [`Orchestrator::gate`]).
fn deferred_flag_of(flag: &Arc<AtomicBool>) -> bool {
    flag.load(Ordering::Relaxed)
}

/// A runtime that exposes no tools — used for `ExecuteDirect` when the acting component holds no
/// `ExecuteMcp` grants at all, so an empty allow-list can't be mistaken for "everything visible"
/// (see `Orchestrator::run`'s `ExecuteDirect` arm).
struct NoMcpRuntime;

#[async_trait]
impl ToolRuntime for NoMcpRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Err("no MCP is granted to this component".into())
    }
}

/// Derive the capability set used to risk-gate a subagent (Decision 4: never widen).
///
/// - Non-empty `decision_capabilities` (tests / future explicit grants):
///   `ceiling ∩ decision_capabilities`.
/// - Empty (normal classifier output — the model is told not to emit capability objects):
///   narrow the ceiling's **`ExecuteMcp` dimension** to `allowed_mcps`, and carry every other
///   capability through unchanged. Empty `allowed_mcps` means no MCP narrowing (same sense as
///   empty `relevant_mcps` on `ExecuteDirect`), so the gate is the full ceiling; the runtime
///   still only exposes registered servers.
///
/// # Why this is not a plain intersection
///
/// It used to be: `ceiling.narrow(&requested)`, where `requested` was built from `allowed_mcps` and
/// therefore contained **only** `ExecuteMcp` entries. `narrow` is a strict intersection, so every
/// `Read(Zone)` and `Write(Zone)` in the ceiling was dropped on the floor — the gate set was
/// `ExecuteMcp`-only, always. No subagent has ever held a zone capability, so every subagent vault
/// write has always been refused and turned into a permission request.
///
/// That went unnoticed for months because **the failure mode is indistinguishable from the success
/// mode**: a denied write raises a proposal, which is exactly what a zone the operator *intended* to
/// protect would produce. The system even logged `'everywhere' grant already present in the overlay
/// — capability=Write(Vault("Learning"))` while refusing that same write, because the pool grant and
/// the gate set were two different values nobody compared.
///
/// The intent was always to scope *which MCPs are reachable* — the doc above said so, and reads
/// were never affected because only writes consult `Write(Zone)`. Scoping MCPs simply is not
/// expressible as an intersection against a set that describes only MCPs. Authority still cannot
/// widen: every capability here comes from `ceiling`.
fn subagent_gate_capabilities(
    ceiling: &CapabilitySet,
    decision_capabilities: &CapabilitySet,
    allowed_mcps: &[String],
) -> CapabilitySet {
    if !decision_capabilities.capabilities.is_empty() {
        return ceiling.narrow(decision_capabilities);
    }
    if allowed_mcps.is_empty() {
        return ceiling.clone();
    }
    CapabilitySet {
        capabilities: ceiling
            .capabilities
            .iter()
            .filter(|cap| match cap {
                // The one dimension this scoping is about.
                Capability::ExecuteMcp(name) => allowed_mcps.contains(name),
                // Zones and AskHuman are not what `allowed_mcps` narrows. Keeping them is not a
                // widening — they are the ceiling's own, and the zone-write-class guard, the
                // consequence guard and the proposal path all still apply on top.
                _ => true,
            })
            .cloned()
            .collect(),
    }
}

/// Build the subagent system prompt, appending its success criteria when present.
fn subagent_instructions(success_criteria: &[String]) -> String {
    if success_criteria.is_empty() {
        return SUBAGENT_PREAMBLE.to_string();
    }
    format!(
        "{SUBAGENT_PREAMBLE}\n\nYou are done when:\n{}",
        format_success_criteria(success_criteria)
    )
}

fn format_success_criteria(success_criteria: &[String]) -> String {
    success_criteria
        .iter()
        .map(|c| format!("- {c}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The three numbers A1 logs, as a pure function of the offered MCP list and the connected
/// catalog: how many were offered, how many distinct MCPs actually appear, and a chars/4 × 1.3
/// token estimate of the serialized schema (same convention as `main-agent`'s compaction).
///
/// Separate from the logging so it is testable without a dispatch — the previous test read these
/// back out of a `#[cfg(test)]` global that production code wrote to, which raced any other test
/// on the same path.
fn catalog_measurements(offered: &[String], catalog: &[ToolDef]) -> (usize, usize, u64) {
    let from_catalog = catalog
        .iter()
        .map(|tool| mcp_of(&tool.name))
        .collect::<HashSet<&str>>()
        .len();
    let est = serde_json::to_string(catalog)
        .map(|s| ((s.len() as f64) / 4.0 * 1.3).ceil() as u64)
        .unwrap_or(0);
    (offered.len(), from_catalog, est)
}

/// Told to a subagent whose report will be **filed verbatim** rather than summarised.
///
/// Without this the subagent has no way to know its `submit_report` summary is the artifact. The
/// `Report::summary` contract says "high-signal, short" and every other dispatch treats it that
/// way, so a research subagent gathers its material, writes a one-line status, and reaches for a
/// vault tool to author the real document — a tool delivery deliberately withheld, because the
/// system performs that write. The first live run filed exactly that: 231 bytes reading "I have
/// all the research I need. Let me now write the comprehensive report directly to the vault."
///
/// So this has to do two things: say the report *is* the document, and pre-empt the search for a
/// writing tool that is not there and is not coming.
fn delivery_directive(path: &str) -> String {
    format!(
        "\n\nOUTPUT CONTRACT — read this before you start.\n\n\
         Your `{SUBMIT_REPORT_TOOL}` `summary` IS the deliverable. The system files it verbatim at \
         `{path}` the moment you submit, and the human reads that file — not a summary of it. So \
         write the summary as the FINISHED DOCUMENT: complete, structured with markdown headings, \
         as long as the material warrants, carrying the detail, sources and specifics you \
         gathered. This is the one case where a long report is the correct answer.\n\n\
         You have no file-writing or vault tool, and you do not need one — do not look for one, and \
         do not end a turn intending to write the document afterwards. There is no afterwards: \
         submitting the report IS writing it. A short status line (\"I have what I need, now I will \
         write it up\") would be filed as the entire document, and the human would receive that \
         instead of your work."
    )
}

/// Told to a research subagent whose report goes back to the **main agent** rather than to a file.
///
/// The sibling of [`delivery_directive`], for the case that has no path to file to. Both exist for
/// the same reason: `Report::summary` is documented to the model as "High-signal, human-readable,
/// short", so absent an instruction otherwise a subagent writes a status line and the material it
/// gathered is discarded at the seam.
///
/// That is not hypothetical here. On 2026-08-02 a delegated research turn returned 504 characters —
/// a third of it session ids — and the face agent, holding source *names* and none of their content,
/// produced 7,872 characters of specific, cited answer from its own priors. The content may well
/// have been right; the claim that it came from those sources was not.
///
/// `delivery_directive` was never reached on this path because it is appended only when
/// `delivery_target` yields a vault path, and a chat `delegate` is `Delivery::Summarize`. The
/// coupling was to *having a file*, when what matters is whether the summary is the deliverable.
///
/// Deliberately does **not** ask for a finished document the way the vault directive does. The main
/// agent is a reader with conversational context, and it decides how much to relay — so this asks
/// for the material, and leaves the shaping to the reader that can see the conversation.
fn relay_directive() -> String {
    format!(
        "

OUTPUT CONTRACT — read this before you start.

         Your `{SUBMIT_REPORT_TOOL}` `summary` is the ONLY thing that leaves this session. The main          agent cannot see your tool calls, your searches, or their results — it sees this summary          and nothing else, and it answers the human from it. Anything you leave out is gone.

         So carry the findings themselves, not a description of having found them. Include the          specifics, figures, comparisons and sources you actually gathered, with enough structure          to be read. \"Comprehensive comparison of X and Y, synthesized from several sources\" is a          status line, not a result: the main agent receiving that has nothing to relay and will          fill the gap from memory, attributing invented detail to sources it never saw.

         Length should follow the material. The main agent will trim or relay as the conversation          needs — that is its job, and it can only do it if you give it something to trim."
    )
}

/// Validate and normalise a `Delivery::Vault` destination, or say why it can't be used.
///
/// The path comes from a model, and it addresses a **write** — so it gets the same treatment every
/// other path-addressed write gets, checked here rather than trusted:
///
/// * It must name a zone (`research/notes.md`, not `notes.md`). A bare filename resolves to no
///   zone, which is precisely the `WriteTarget::Undeterminable` case the capability guard fails
///   closed on; accepting it here would route a report somewhere nobody authorised.
/// * It must not be absolute, and must not contain `..`. Neither is expressible as a vault-relative
///   path, so both are attempts — deliberate or hallucinated — to land outside the vault.
///
/// Returns the normalised path (backslashes folded to `/`, no leading slash) on success. Failure is
/// a downgrade to `Summarize`, never an error: a bad path should cost the human a nicer delivery,
/// not the research they waited for.
fn vault_delivery_path(path: &str) -> Result<String, &'static str> {
    let cleaned = path.trim().replace('\\', "/");
    let cleaned = cleaned.trim_start_matches('/').trim();
    if cleaned.is_empty() {
        return Err("delivery path is empty");
    }
    if cleaned.contains(':') {
        return Err("delivery path looks absolute (drive-qualified)");
    }
    if cleaned.split('/').any(|seg| seg == ".." || seg.is_empty()) {
        return Err("delivery path contains an empty or parent (`..`) segment");
    }
    match cleaned.split_once('/') {
        Some((zone, rest)) if !zone.is_empty() && !rest.is_empty() => Ok(cleaned.to_string()),
        _ => Err("delivery path names no zone (expected `<zone>/<name>.md`)"),
    }
}

/// Minimum plausible size for a report being filed as a standalone document.
///
/// Not a quality bar — a deliberately low floor that only a *non*-document trips. The failure it
/// catches filed 231 bytes; a genuine short answer that belongs in the vault still clears this, and
/// a genuine long one clears it by orders of magnitude (live runs: 19,970 and 28,225 bytes). Set it
/// higher and it starts rejecting real work, which would be worse than the bug.
const MIN_DELIVERED_DOCUMENT_BYTES: usize = 400;

/// Does this report look like the document it is about to be filed as?
///
/// Scoped to the `Delivery::Vault` path on purpose. A general "is this output good" check would need
/// a threshold that is wrong somewhere — 400 bytes is absurd for "what's on my calendar tomorrow",
/// which is a perfectly correct one-line answer. But vault delivery has a *stated contract*
/// (`delivery_directive`: your report IS the finished document), so this only asks whether that
/// contract was honoured. That is why it needs no per-task tuning.
fn looks_like_a_document(summary: &str) -> Result<(), &'static str> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return Err("it is empty");
    }
    if trimmed.len() < MIN_DELIVERED_DOCUMENT_BYTES {
        return Err("it is too short to be a write-up");
    }
    // The specific shape that got filed: a couple of sentences of the model talking about what it
    // is going to do. A real write-up of this length has structure — headings, bullets, or simply
    // more than a few lines. Checked only as a fallback for something long enough to pass the byte
    // floor but still not a document.
    let has_structure = trimmed.contains('#')
        || trimmed.contains("\n-")
        || trimmed.contains("\n*")
        || trimmed.contains("\n1.")
        || trimmed.lines().filter(|l| !l.trim().is_empty()).count() >= 5;
    if !has_structure {
        return Err("it has no headings, list, or paragraph structure");
    }
    Ok(())
}

/// The note written for a `Delivery::Vault` report.
///
/// Front-matter first, because this file is found later, out of context, with no conversation
/// around it — "where did this come from and can I trust it" has to be answerable from the note
/// itself. The correlation id is the same one every tool write in the run was tagged with, so the
/// note joins back to the dispatch journal and the audit log.
fn vault_note_body(report: &Report, correlation_id: &str, source: &str) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("liberado_source: ");
    out.push_str(source);
    out.push('\n');
    out.push_str("liberado_correlation: ");
    out.push_str(correlation_id);
    out.push('\n');
    out.push_str("generated: ");
    out.push_str(&chrono::Utc::now().to_rfc3339());
    out.push_str("\n---\n\n");
    out.push_str(report.summary.trim());
    out.push('\n');

    if !report.new_high_signal_facts.is_empty() {
        out.push_str("\n## Notable\n\n");
        for fact in &report.new_high_signal_facts {
            out.push_str("- ");
            out.push_str(fact);
            out.push('\n');
        }
    }
    if let Some(follow_up) = &report.follow_up {
        out.push_str("\n## Suggested next step\n\n");
        out.push_str(follow_up.trim());
        out.push('\n');
    }
    out
}

/// What the main agent gets instead of the report body.
///
/// The receipt is the mechanism, not a courtesy: the token and latency saving comes from the main
/// agent having **nothing to restate**, so this deliberately does not summarise the findings. The
/// closing instruction is there because a main agent handed a bare path tends to invent a summary
/// of what it thinks is in the file — it has to be told that reading it is a separate, optional act.
fn vault_receipt(path: &str, bytes: usize) -> String {
    format!(
        "Filed to the vault at `{path}` ({bytes} bytes). The findings are in that note — they were \
         written directly and are NOT reproduced here. Tell the human where it is; do not \
         characterise or summarise its contents, and read the note first if they ask about them."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_executor::SUBMIT_REPORT_TOOL;
    use liberado_provider::{CompletionResponse, MockProvider};
    use liberado_test_support::CallRecordingFactory;

    /// Serializes tests that touch process-global tracing subscriber state or
    /// module-level `LAST_CATALOG_*` atomics. Two tests racing on either of
    /// these surfaces as intermittent failures (#B3).
    static TRACING_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn subagent_instructions_with_criteria() {
        let criteria = vec!["find the answer".into(), "write it down".into()];
        let result = subagent_instructions(&criteria);
        assert!(result.contains("find the answer"));
        assert!(result.contains("write it down"));
        assert!(result.contains(SUBAGENT_PREAMBLE));
    }

    #[test]
    fn disposition_reports_the_runtime_deferral_flag() {
        // Gap 2: `deferred_to_human()` surfaces a `Reported`'s runtime flag; other dispositions are
        // never "already notified out-of-band" here (a `Propose`'s ping is sent by the writer).
        let report = |deferred: bool| Report {
            outcome: Outcome::Succeeded,
            summary: "s".into(),
            artifacts: vec![],
            new_high_signal_facts: vec![],
            follow_up: None,
            deferred_to_human: deferred,
            repeat_calls: 0,
        };
        assert!(Disposition::Reported(report(true)).deferred_to_human());
        assert!(!Disposition::Reported(report(false)).deferred_to_human());
        assert!(
            !Disposition::Clarify {
                questions: vec!["?".into()],
                what_blocked: BlockReason::Ambiguous,
            }
            .deferred_to_human()
        );
    }

    /// The exact string that was filed live, as the fixture. The prompt fix makes this unlikely;
    /// this makes it impossible to file.
    #[test]
    fn the_231_byte_non_document_is_refused() {
        let filed_live = "I have all the research I need. Let me now write the comprehensive \
                          report directly to the vault.";
        assert!(looks_like_a_document(filed_live).is_err());
        assert!(looks_like_a_document("").is_err());
        assert!(looks_like_a_document("   \n  ").is_err());
    }

    /// The floor must not reject real work — that would be worse than the bug it guards.
    #[test]
    fn genuine_write_ups_pass_verification() {
        // A structured report, comfortably over the floor.
        let mut doc = String::from("# Findings\n\n");
        for i in 0..12 {
            doc.push_str(&format!(
                "## Section {i}\n\nThe implementations converged on a shared interface, and \
                 adoption followed once tooling stabilised.\n\n"
            ));
        }
        assert!(looks_like_a_document(&doc).is_ok());

        // Long prose with no markdown, but real paragraph structure, still passes: the check is
        // "is this a document", not "is this formatted the way I like".
        let prose = (0..8)
            .map(|i| format!("Paragraph {i} carries a full sentence of genuine findings here."))
            .collect::<Vec<_>>()
            .join("\n\n");
        assert!(prose.len() > MIN_DELIVERED_DOCUMENT_BYTES);
        assert!(looks_like_a_document(&prose).is_ok());
    }

    /// Long enough to clear the byte floor but still just the model talking — caught by structure.
    #[test]
    fn a_long_status_line_without_structure_is_still_refused() {
        let rambling = "I have now completed all of the research that I believe is necessary for \
                        this task and I am confident that I have gathered sufficient material from \
                        the sources I consulted, so the next thing I intend to do is compose the \
                        full write-up and place it into the vault at the path that was specified \
                        for me earlier in this conversation, which I will begin doing immediately \
                        once I have finished organising my notes into a coherent narrative order.";
        assert!(rambling.len() > MIN_DELIVERED_DOCUMENT_BYTES);
        assert!(looks_like_a_document(rambling).is_err());
    }

    /// The 231-byte regression. A subagent whose report is filed verbatim must be told before it
    /// starts, or it writes a status line and waits to author the document with a tool it never
    /// had — which is exactly what the first live run filed.
    #[test]
    fn a_delivered_subagent_is_told_its_report_is_the_document() {
        let orch = delivering_orchestrator();
        let target = orch
            .delivery_target(&vault_to("Learning/x.md"), &read_only_mcps())
            .expect("this dispatch can deliver");
        let directive = delivery_directive(&target);
        assert!(directive.contains("Learning/x.md"));
        assert!(
            directive.contains(SUBMIT_REPORT_TOOL),
            "it must name the tool whose argument becomes the artifact"
        );
        // The two failure modes it exists to prevent, in the model's own words.
        assert!(directive.contains("no file-writing or vault tool"));
        assert!(directive.contains("There is no afterwards"));
    }

    /// The **document** contract is only correct when delivery will actually happen — a downgraded
    /// dispatch must not be told its summary is a filed artifact, because no file is written.
    ///
    /// It does still get the *relay* contract when it is research (see `relay_directive`): the
    /// summary is the only thing that reaches the main agent either way, so it must carry the
    /// material. The two differ in what they ask for — a finished document versus the findings,
    /// shaped by whoever relays them — not in whether the subagent is told anything at all.
    #[test]
    fn a_dispatch_that_cannot_deliver_gets_no_document_contract() {
        let orch = delivering_orchestrator();
        assert!(
            orch.delivery_target(&vault_to("Learning/x.md"), &acting_mcps())
                .is_err(),
            "an acting dispatch cannot deliver, so it must not be given the contract"
        );
        assert!(
            orch.delivery_target(&Delivery::Summarize, &read_only_mcps())
                .is_err(),
            "the default sink authors no document"
        );
    }

    /// The seam that discarded delegated work: a research subagent reporting back to the main
    /// agent must be told the summary is the only thing that leaves the session.
    ///
    /// Without it, `Report::summary`'s own schema ("High-signal, human-readable, short") is the only
    /// instruction it has, and it files a status line — which is what produced 504 characters of
    /// "comprehensive comparison, synthesized from authoritative sources" while the face agent
    /// invented 7,872 characters of specifics and attributed them to sources nothing had read.
    #[test]
    fn a_research_subagent_reporting_to_chat_is_told_the_summary_is_the_material() {
        let directive = relay_directive();
        assert!(
            directive.contains(SUBMIT_REPORT_TOOL),
            "it must name the tool whose argument is the only thing that escapes"
        );
        // The fact the model cannot otherwise know.
        assert!(directive.contains("cannot see your tool calls"));
        assert!(directive.contains("Anything you leave out is gone"));
        // Names the exact failure, so a model pattern-matching on it recognises its own draft.
        assert!(directive.contains("status line, not a result"));
    }

    /// It asks for material, NOT for a finished document — that distinction is the whole reason it
    /// is a separate directive. The main agent has the conversation and does the shaping; a
    /// subagent told to write the final artifact would pre-empt a decision it lacks the context to
    /// make, and drop a report into a chat that wanted two sentences.
    #[test]
    fn the_relay_contract_leaves_shaping_to_the_main_agent() {
        let relay = relay_directive();
        let document = delivery_directive("Learning/x.md");
        assert!(
            document.contains("FINISHED DOCUMENT"),
            "the vault contract asks for the artifact itself"
        );
        assert!(
            !relay.contains("FINISHED DOCUMENT"),
            "the relay contract must not ask for a finished artifact"
        );
        assert!(
            relay.contains("will trim or relay"),
            "it has to say who does the shaping, or the subagent guesses"
        );
    }

    /// The wiring, not the wording. Both directives are pure functions and easy to assert on, which
    /// is exactly how a thorough set of tests can cover text that is never appended to anything —
    /// deleting the call site failed nothing until this existed.
    #[test]
    fn the_output_contract_matches_where_the_report_is_going() {
        let orch = delivering_orchestrator();

        let filed = orch.output_contract(&vault_to("Learning/x.md"), &read_only_mcps(), true);
        assert!(
            filed.contains("FINISHED DOCUMENT"),
            "a report that will be filed gets the document contract"
        );

        let relayed = orch.output_contract(&Delivery::Summarize, &read_only_mcps(), true);
        assert!(
            relayed.contains("Anything you leave out is gone"),
            "research relayed to the main agent gets the relay contract"
        );

        // Action work produces its own artifact, so a short status is genuinely the right report.
        let acting = orch.output_contract(&Delivery::Summarize, &acting_mcps(), false);
        assert!(
            acting.is_empty(),
            "an acting dispatch gets no contract, or every chat gets a wall of text: {acting}"
        );
    }

    #[test]
    fn subagent_instructions_empty_returns_preamble() {
        let result = subagent_instructions(&[]);
        assert_eq!(result, SUBAGENT_PREAMBLE);
    }

    #[test]
    fn subagent_gate_derives_from_allowed_mcps_when_capabilities_empty() {
        let ceiling = CapabilitySet::from_iter([
            Capability::ExecuteMcp("turbovault".into()),
            Capability::ExecuteMcp("weather".into()),
        ]);
        let gate =
            subagent_gate_capabilities(&ceiling, &CapabilitySet::empty(), &["turbovault".into()]);
        assert!(gate.grants_mcp("turbovault"));
        assert!(!gate.grants_mcp("weather"));
    }

    /// The months-old bug: `narrow` is a strict intersection, and the set built from
    /// `allowed_mcps` holds only `ExecuteMcp`, so every zone capability was dropped. No subagent
    /// ever held `Write`, so every subagent vault write became a permission request — and that
    /// looked exactly like a zone the operator meant to protect, which is why nobody noticed.
    #[test]
    fn scoping_mcps_does_not_strip_zone_authority() {
        let ceiling = CapabilitySet::from_iter([
            Capability::ExecuteMcp("turbovault".into()),
            Capability::ExecuteMcp("email".into()),
            Capability::Read(liberado_common::Zone::vault("Learning")),
            Capability::Write(liberado_common::Zone::vault("Learning")),
        ]);
        let gate =
            subagent_gate_capabilities(&ceiling, &CapabilitySet::empty(), &["turbovault".into()]);

        // The dimension `allowed_mcps` is actually about.
        assert!(gate.grants_mcp("turbovault"));
        assert!(
            !gate.grants_mcp("email"),
            "out-of-scope MCPs are still removed"
        );

        // The dimension it is not about, and used to destroy.
        assert!(gate.contains(&Capability::Write(liberado_common::Zone::vault("Learning"))));
        assert!(gate.contains(&Capability::Read(liberado_common::Zone::vault("Learning"))));
    }

    /// Decision 4 still holds: everything in the gate set comes from the ceiling.
    #[test]
    fn scoping_can_never_invent_authority_the_ceiling_lacks() {
        let ceiling = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
        let gate =
            subagent_gate_capabilities(&ceiling, &CapabilitySet::empty(), &["turbovault".into()]);
        assert!(!gate.contains(&Capability::Write(liberado_common::Zone::vault("Learning"))));
        assert_eq!(gate.capabilities.len(), 1);
    }

    #[test]
    fn subagent_gate_empty_allowed_mcps_uses_full_ceiling() {
        let ceiling = CapabilitySet::from_iter([
            Capability::ExecuteMcp("turbovault".into()),
            Capability::ExecuteMcp("weather".into()),
        ]);
        let gate = subagent_gate_capabilities(&ceiling, &CapabilitySet::empty(), &[]);
        assert!(gate.grants_mcp("turbovault"));
        assert!(gate.grants_mcp("weather"));
    }

    #[test]
    fn subagent_gate_explicit_capabilities_intersect_ceiling() {
        let ceiling = CapabilitySet::from_iter([
            Capability::ExecuteMcp("a".into()),
            Capability::ExecuteMcp("b".into()),
        ]);
        let explicit = CapabilitySet::from_iter([Capability::ExecuteMcp("a".into())]);
        // allowed_mcps ignored when explicit capabilities are present
        let gate = subagent_gate_capabilities(&ceiling, &explicit, &["b".into()]);
        assert!(gate.grants_mcp("a"));
        assert!(!gate.grants_mcp("b"));
    }

    fn orchestrator_with_catalog(catalog: Vec<(String, Consequence)>) -> Orchestrator {
        Orchestrator::new(
            Arc::new(MockProvider::with_script("mock", vec![])),
            NoopFactory,
            CapabilitySet::empty(),
            catalog,
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
            "default",
        )
    }

    /// An orchestrator that *can* deliver: a sink, a writable `Learning` zone, and the `Write`
    /// capability for it. Every delivery test starts from this and removes one thing.
    fn delivering_orchestrator() -> Orchestrator {
        Orchestrator::new(
            Arc::new(MockProvider::with_script("mock", vec![])),
            NoopFactory,
            CapabilitySet::from_iter([Capability::Write(liberado_common::Zone::vault("Learning"))]),
            web_catalog(),
            Vec::new(),
            vec![
                ("Learning".into(), WriteClass::AgentWritable),
                ("Legal".into(), WriteClass::HumanOnly),
            ],
            std::env::temp_dir(),
            ProposalSigner::random(),
            "default",
        )
        .with_report_sink(ReportSink::new("vault", "write_note", "path", "content"))
    }

    /// A dispatch whose MCPs cannot reach past the vault: `ReadOnly` lookups and a `Reversible`
    /// vault write. Both sit below `CONSEQUENCE_GATE`, so nothing happened that the main agent
    /// must narrate.
    fn read_only_mcps() -> Vec<String> {
        vec!["search".into(), "vault".into()]
    }

    /// A dispatch that can act on the world — `email` is `External`.
    fn acting_mcps() -> Vec<String> {
        vec!["search".into(), "email".into()]
    }

    fn vault_to(path: &str) -> Delivery {
        Delivery::Vault { path: path.into() }
    }

    /// The happy path: read-only work, clean outcome, writable zone we hold `Write` on.
    #[test]
    fn a_clean_read_only_run_is_delivered_to_the_vault() {
        let orch = delivering_orchestrator();
        let (effective, downgrade) = orch.resolve_delivery(
            &vault_to("Learning/x.md"),
            &read_only_mcps(),
            Outcome::Succeeded,
        );
        assert_eq!(effective, vault_to("Learning/x.md"));
        assert_eq!(downgrade, None);
    }

    /// Guard #1. The whole point of the read-only condition: if the subagent could have *acted*,
    /// the main agent narrates it, because it is the only participant that can re-dispatch or
    /// explain a half-done action.
    #[test]
    fn a_dispatch_that_can_act_never_bypasses_the_main_agent() {
        let orch = delivering_orchestrator();
        let (effective, downgrade) = orch.resolve_delivery(
            &vault_to("Learning/x.md"),
            &acting_mcps(),
            Outcome::Succeeded,
        );
        assert_eq!(effective, Delivery::Summarize);
        assert!(downgrade.is_some_and(|d| d.contains("act outside the vault")));
    }

    /// A caller-supplied ceiling replaces the depth's, on every path.
    ///
    /// The budgets are deployment constants and the *path* is chosen by the dispatcher from goal
    /// phrasing, so before this a schedule could neither raise its budget nor predict which one it
    /// would get. Observed live: an inbox schedule routed to `ExecuteDirect` spent all four turns
    /// reading the vault and filed nothing.
    #[test]
    fn a_supplied_turn_budget_replaces_the_one_depth_would_choose() {
        let orch = delivering_orchestrator();

        for depth in [Depth::Deep, Depth::Normal, Depth::Shallow] {
            let base = orch.budget_for(depth);
            assert_eq!(
                orch.effective_budget(base, Some(25)).max_turns,
                25,
                "an override must win on every path, not just the subagent one"
            );
        }
        // The direct path has no `Depth`; it must honour the override too.
        assert_eq!(
            orch.effective_budget(&orch.direct_budget, Some(25))
                .max_turns,
            25
        );
    }

    /// Absent — and zero, which `GoalSpec::max_turns` already uses to mean "pack default" — must
    /// leave the configured ceiling exactly as it was.
    #[test]
    fn no_override_leaves_the_configured_budget_untouched() {
        let orch = delivering_orchestrator();
        let configured = orch.budget_for(Depth::Normal).max_turns;

        assert_eq!(
            orch.effective_budget(orch.budget_for(Depth::Normal), None)
                .max_turns,
            configured
        );
        assert_eq!(
            orch.effective_budget(orch.budget_for(Depth::Normal), Some(0))
                .max_turns,
            configured,
            "0 means `pack default`, not `no turns` — treating it literally would deadlock the run"
        );
    }

    /// Raising the turn ceiling must not quietly discard the other limits an operator set.
    #[test]
    fn an_override_preserves_extra_limits() {
        use liberado_executor::WallClockLimit;
        let base = Budget::new(4).with_limit(WallClockLimit(std::time::Duration::from_secs(60)));
        let raised = base.clone().with_max_turns(40);

        assert_eq!(raised.max_turns, 40);
        assert_eq!(
            raised.extra_limit_count(),
            base.extra_limit_count(),
            "wall-clock and token limits must survive a turn-cap change"
        );
        assert_eq!(
            base.extra_limit_count(),
            1,
            "fixture must actually carry a limit"
        );
    }

    /// The Telegram failure, root cause. A deep-research goal that merely *mentioned* the vault
    /// got the vault MCP in `allowed_mcps`, which made it "not read-only", which gave it 8 turns
    /// instead of 30 — and it failed at the budget. Depth is a property of the task, not of which
    /// MCPs it happens to touch.
    #[test]
    fn depth_sets_the_budget_regardless_of_which_mcps_are_in_scope() {
        let orch = delivering_orchestrator();
        let with_vault = ["search".to_string(), "vault".to_string()];

        assert_eq!(
            orch.budget_for(Depth::Deep).max_turns,
            orch.research_budget.max_turns,
            "a deep task gets the research budget even though `vault` is Reversible"
        );
        assert_eq!(
            orch.budget_for(Depth::Normal).max_turns,
            orch.subagent_budget.max_turns
        );
        assert!(
            orch.budget_for(Depth::Shallow).max_turns < orch.budget_for(Depth::Normal).max_turns
        );

        // The old predicate still says "not read-only" for this list — and no longer decides turns.
        assert!(!orch.is_read_only_dispatch(&with_vault));
    }

    /// Salvage stays *inferred*, and from consequence rather than depth: returning partial findings
    /// is safe when nothing irreversible could have happened. It is a safety property, not a
    /// preference, so it is not a model's to declare.
    #[test]
    fn salvage_follows_consequence_not_depth() {
        let orch = delivering_orchestrator();
        assert!(orch.delivery_consequence_ok(&["search".into(), "vault".into()]));
        assert!(!orch.delivery_consequence_ok(&["search".into(), "email".into()]));
    }

    /// The regression that motivated splitting delivery from the budget derivation.
    ///
    /// "Research X and save it to my vault" is the clearest case for direct delivery, and it is
    /// exactly the phrasing that makes a classifier include the vault MCP. Live, that made the
    /// dispatch non-read-only and delivery refused — the feature switched itself off precisely
    /// when the human had asked for it most plainly. A `Reversible` vault write left nothing
    /// outside the system, so there is nothing for the main agent to narrate.
    #[test]
    fn a_vault_reading_dispatch_may_still_deliver() {
        let orch = delivering_orchestrator();
        let (effective, downgrade) = orch.resolve_delivery(
            &vault_to("Learning/x.md"),
            &["search".into(), "vault".into()],
            Outcome::Succeeded,
        );
        assert_eq!(effective, vault_to("Learning/x.md"));
        assert_eq!(downgrade, None);
    }

    /// ...but the budget derivation still says "not research" for that same dispatch. The two
    /// predicates ask different questions of the same list and are *supposed* to disagree: turns
    /// and salvage care whether anything could be left half-written; delivery cares whether
    /// something happened out in the world.
    #[test]
    fn delivery_and_budget_derivations_are_independent() {
        let orch = delivering_orchestrator();
        let vault_read = ["search".to_string(), "vault".to_string()];
        assert!(
            !orch.is_read_only_dispatch(&vault_read),
            "a Reversible MCP is not read-only — the budget derivation is unchanged"
        );
        assert!(
            orch.delivery_consequence_ok(&vault_read),
            "but nothing irreversible happened, so delivery is still allowed"
        );
    }

    /// An unbounded scope (empty = the full ceiling) cannot be rated, so it fails closed.
    #[test]
    fn an_unscoped_dispatch_may_not_bypass_the_main_agent() {
        let orch = delivering_orchestrator();
        assert!(!orch.delivery_consequence_ok(&[]));
        assert!(!orch.delivery_consequence_ok(&["who-is-this".into()]));
    }

    /// Guard #2. A failed or partial run is exactly when the detail belongs in the conversation —
    /// and filing a half-finished write-up under a name implying a finished document is its own
    /// small harm. Nothing is lost: the findings still reach the human, just narrated.
    #[test]
    fn only_a_clean_success_is_filed() {
        let orch = delivering_orchestrator();
        for outcome in [
            Outcome::Failed,
            Outcome::PartiallySucceeded,
            Outcome::Proposed,
        ] {
            let (effective, downgrade) =
                orch.resolve_delivery(&vault_to("Learning/x.md"), &read_only_mcps(), outcome);
            assert_eq!(
                effective,
                Delivery::Summarize,
                "{outcome:?} must not be filed"
            );
            assert!(downgrade.is_some());
        }
    }

    /// `deliver_to_vault` skips the `RiskGatedToolRuntime`, so the rules that runtime would have
    /// applied are applied here instead. Without this the delivery path is an unguarded write
    /// straight into the vault — a guard silently absent because a new code path grew around it.
    #[test]
    fn a_zone_that_is_not_agent_writable_is_refused_not_proposed() {
        let orch = delivering_orchestrator();
        let (effective, downgrade) = orch.resolve_delivery(
            &vault_to("Legal/x.md"),
            &read_only_mcps(),
            Outcome::Succeeded,
        );
        assert_eq!(effective, Delivery::Summarize);
        assert!(downgrade.is_some_and(|d| d.contains("agent-writable")));
    }

    /// An undeclared zone inherits `WriteClass`'s `ProposalOnly` default, so a hallucinated
    /// destination is refused rather than created. This is the case that fires when the classifier
    /// invents `research/` in a vault that has no such zone.
    #[test]
    fn an_undeclared_zone_is_refused_by_the_fail_safe_default() {
        let orch = delivering_orchestrator();
        let (effective, _) = orch.resolve_delivery(
            &vault_to("research/x.md"),
            &read_only_mcps(),
            Outcome::Succeeded,
        );
        assert_eq!(effective, Delivery::Summarize);
    }

    /// The orchestrator writes under its own authority, so it must hold that authority.
    #[test]
    fn delivery_requires_write_on_the_target_zone() {
        let mut orch = delivering_orchestrator();
        orch.capabilities = CapabilitySet::empty();
        let (effective, downgrade) = orch.resolve_delivery(
            &vault_to("Learning/x.md"),
            &read_only_mcps(),
            Outcome::Succeeded,
        );
        assert_eq!(effective, Delivery::Summarize);
        assert!(downgrade.is_some_and(|d| d.contains("Write capability")));
    }

    /// An unconfigured deployment behaves exactly as it did before delivery existed — it does not
    /// fail, it just summarizes.
    #[test]
    fn without_a_sink_delivery_quietly_falls_back() {
        let orch = orchestrator_with_catalog(web_catalog());
        let (effective, downgrade) = orch.resolve_delivery(
            &vault_to("Learning/x.md"),
            &read_only_mcps(),
            Outcome::Succeeded,
        );
        assert_eq!(effective, Delivery::Summarize);
        assert!(downgrade.is_some_and(|d| d.contains("no report sink")));
    }

    #[test]
    fn summarize_is_never_downgraded_and_needs_nothing() {
        let orch = orchestrator_with_catalog(web_catalog());
        for (mcps, outcome) in [
            (&read_only_mcps(), Outcome::Succeeded),
            (&acting_mcps(), Outcome::Failed),
        ] {
            let (effective, downgrade) = orch.resolve_delivery(&Delivery::Summarize, mcps, outcome);
            assert_eq!(effective, Delivery::Summarize);
            assert_eq!(downgrade, None);
        }
    }

    /// The path is model-produced and addresses a write, so it is checked, not trusted. A bare
    /// filename resolves to no zone — the `WriteTarget::Undeterminable` case the capability guard
    /// fails closed on — and `..`/absolute paths are attempts to land outside the vault.
    #[test]
    fn delivery_paths_that_name_no_zone_or_escape_are_rejected() {
        for bad in [
            "",
            "   ",
            "notes.md",
            "/",
            "Learning/../../etc/passwd",
            "C:/Windows/x.md",
            "Learning//x.md",
            "Learning/",
        ] {
            assert!(
                vault_delivery_path(bad).is_err(),
                "{bad:?} should not be a deliverable path"
            );
        }
    }

    #[test]
    fn delivery_paths_are_normalised_not_merely_accepted() {
        assert_eq!(
            vault_delivery_path("/Learning/deep/x.md").unwrap(),
            "Learning/deep/x.md"
        );
        assert_eq!(
            vault_delivery_path(r"Learning\x.md").unwrap(),
            "Learning/x.md"
        );
    }

    /// The note is found later, out of context, with no conversation around it — "where did this
    /// come from" has to be answerable from the note itself.
    #[test]
    fn the_delivered_note_carries_its_own_provenance() {
        let report = Report {
            outcome: Outcome::Succeeded,
            summary: "## Findings\n\nGraphs are hard.".into(),
            artifacts: vec![],
            new_high_signal_facts: vec!["cost scales superlinearly".into()],
            follow_up: Some("benchmark it".into()),
            deferred_to_human: false,
            repeat_calls: 0,
        };
        let body = vault_note_body(&report, "corr-42", "liberado-executor");
        assert!(body.starts_with("---\n"), "front matter comes first");
        assert!(body.contains("liberado_correlation: corr-42"));
        assert!(body.contains("liberado_source: liberado-executor"));
        // The body is the point — verbatim, not paraphrased.
        assert!(body.contains("Graphs are hard."));
        assert!(body.contains("cost scales superlinearly"));
        assert!(body.contains("benchmark it"));
    }

    /// The saving comes from the main agent having nothing to restate, so the receipt must not
    /// contain the findings — and must say so, or the model invents a summary of a file it never
    /// read.
    #[test]
    fn the_receipt_withholds_the_body_and_says_so() {
        let receipt = vault_receipt("Learning/x.md", 4096);
        assert!(receipt.contains("Learning/x.md"));
        assert!(receipt.contains("4096"));
        assert!(receipt.contains("NOT reproduced here"));
    }

    fn web_catalog() -> Vec<(String, Consequence)> {
        vec![
            ("search".into(), Consequence::ReadOnly),
            ("spider".into(), Consequence::ReadOnly),
            ("vault".into(), Consequence::Reversible),
            ("email".into(), Consequence::External),
        ]
    }

    /// "Research" is derived, not declared: every MCP the subagent may touch is read-only, so
    /// nothing can be left half-written. This is the condition that earns both the long budget and
    /// the right to file partial findings.
    #[test]
    fn read_only_dispatch_is_research() {
        let orch = orchestrator_with_catalog(web_catalog());
        assert!(orch.is_read_only_dispatch(&["search".into()]));
        assert!(orch.is_read_only_dispatch(&["search".into(), "spider".into()]));
    }

    #[test]
    fn any_writer_in_the_set_disqualifies_it() {
        let orch = orchestrator_with_catalog(web_catalog());
        // One writer is enough: the run could leave something half-done.
        assert!(!orch.is_read_only_dispatch(&["search".into(), "vault".into()]));
        assert!(!orch.is_read_only_dispatch(&["search".into(), "email".into()]));
    }

    /// Fail toward the ordinary budget. If the catalog and the classifier ever disagree about a
    /// name, the harmless outcome is research running with 8 turns — not an unrecognised writer
    /// getting 30 turns and being told its partial work is worth reporting.
    #[test]
    fn an_unknown_mcp_is_not_treated_as_read_only() {
        let orch = orchestrator_with_catalog(web_catalog());
        assert!(!orch.is_read_only_dispatch(&["search".into(), "who-is-this".into()]));
    }

    /// An empty set is not research either — there is nothing to have read.
    #[test]
    fn an_empty_mcp_set_is_not_research() {
        let orch = orchestrator_with_catalog(web_catalog());
        assert!(!orch.is_read_only_dispatch(&[]));
    }

    #[test]
    fn research_budget_is_larger_than_the_general_subagent_budget() {
        let orch = orchestrator_with_catalog(web_catalog());
        assert!(
            orch.research_budget.max_turns > orch.subagent_budget.max_turns,
            "gathering work is turn-hungry in a way acting work is not"
        );
        assert_eq!(orch.research_budget.max_turns, RESEARCH_MAX_TURNS);
    }

    #[test]
    fn with_research_budget_overrides_the_default() {
        let orch = orchestrator_with_catalog(Vec::new()).with_research_budget(Budget::new(12));
        assert_eq!(orch.research_budget.max_turns, 12);
    }

    #[test]
    fn with_source_overrides_default() {
        let provider = Arc::new(MockProvider::with_script("mock", vec![]));
        let orch = Orchestrator::new(
            provider,
            NoopFactory,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
            "default",
        )
        .with_source("custom-source");
        assert_eq!(orch.source, "custom-source");
    }

    struct NoopFactory;

    #[async_trait]
    impl RuntimeFactory for NoopFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            unreachable!("with_source test never calls run")
        }
    }

    // ------------------------------------------------------------------
    // dispatch_parallel tests
    // ------------------------------------------------------------------

    fn submit_report_response(summary: &str, outcome: &str) -> CompletionResponse {
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c",
            SUBMIT_REPORT_TOOL,
            serde_json::json!({
                "outcome": outcome,
                "summary": summary,
                "artifacts": [],
                "new_high_signal_facts": [],
            }),
        )])
    }

    #[tokio::test]
    async fn dispatch_parallel_spawns_multiple_subagents() {
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                submit_report_response("task A done", "succeeded"),
                submit_report_response("task B done", "succeeded"),
            ],
        ));
        let factory = CallRecordingFactory::default();
        let calls = factory.calls.clone();
        let orch = Orchestrator::new(
            provider,
            factory,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
            "default",
        );

        let sub_dispatches = vec![
            SubDispatch {
                goal: "do A".into(),
                allowed_mcps: vec!["mcp-a".into()],
                success_criteria: vec![],
                correlation_id: "corr-a".into(),
                label: "A".into(),
                workspace_root: None,
            },
            SubDispatch {
                goal: "do B".into(),
                allowed_mcps: vec!["mcp-b".into()],
                success_criteria: vec![],
                correlation_id: "corr-b".into(),
                label: "B".into(),
                workspace_root: None,
            },
        ];

        let report = orch
            .dispatch_parallel(sub_dispatches, 2)
            .await
            .expect("dispatch_parallel");

        // Both runtime_for calls should have been made
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        // Verify the scoped MCPs match what each sub-dispatch requested
        assert_eq!(calls[0].0, vec!["mcp-a"]);
        assert_eq!(calls[1].0, vec!["mcp-b"]);

        // Verify the report merged both summaries
        assert!(
            report.summary.contains("task A done"),
            "summary: {}",
            report.summary
        );
        assert!(
            report.summary.contains("task B done"),
            "summary: {}",
            report.summary
        );
        assert_eq!(report.outcome, Outcome::Succeeded);
    }

    #[tokio::test]
    async fn dispatch_parallel_merges_reports() {
        // First sub-agent succeeds with artifacts + facts
        let resp_a = CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c",
            SUBMIT_REPORT_TOOL,
            serde_json::json!({
                "outcome": "succeeded",
                "summary": "task A done",
                "artifacts": ["/path/a.md"],
                "new_high_signal_facts": ["fact A"],
            }),
        )]);
        // Second sub-agent partially succeeds with different artifacts + facts
        let resp_b = CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c",
            SUBMIT_REPORT_TOOL,
            serde_json::json!({
                "outcome": "partially_succeeded",
                "summary": "task B partial",
                "artifacts": ["/path/b.md"],
                "new_high_signal_facts": ["fact B"],
            }),
        )]);

        let provider = Arc::new(MockProvider::with_script("mock", [resp_a, resp_b]));
        let factory = CallRecordingFactory::default();
        let orch = Orchestrator::new(
            provider,
            factory,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
            "default",
        );

        let sub_dispatches = vec![
            SubDispatch {
                goal: "do A".into(),
                allowed_mcps: vec![],
                success_criteria: vec![],
                correlation_id: "corr-a".into(),
                label: "A".into(),
                workspace_root: None,
            },
            SubDispatch {
                goal: "do B".into(),
                allowed_mcps: vec![],
                success_criteria: vec![],
                correlation_id: "corr-b".into(),
                label: "B".into(),
                workspace_root: None,
            },
        ];

        let report = orch
            .dispatch_parallel(sub_dispatches, 2)
            .await
            .expect("dispatch_parallel");

        // Summaries from both should appear
        assert!(report.summary.contains("task A done"));
        assert!(report.summary.contains("task B partial"));
        // Artifacts and facts are merged
        assert_eq!(report.artifacts, vec!["/path/a.md", "/path/b.md"]);
        assert_eq!(report.new_high_signal_facts, vec!["fact A", "fact B"]);
        // Overall outcome reflects partial failure
        assert_eq!(report.outcome, Outcome::PartiallySucceeded);
    }

    #[tokio::test]
    async fn dispatch_parallel_semaphore_limits_concurrency() {
        // Use max_concurrent=1 to verify sequential execution still works
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                submit_report_response("task 1", "succeeded"),
                submit_report_response("task 2", "succeeded"),
            ],
        ));
        let factory = CallRecordingFactory::default();
        let calls = factory.calls.clone();
        let orch = Orchestrator::new(
            provider,
            factory,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
            "default",
        );

        let sub_dispatches = vec![
            SubDispatch {
                goal: "task 1".into(),
                allowed_mcps: vec![],
                success_criteria: vec![],
                correlation_id: "c1".into(),
                label: "1".into(),
                workspace_root: None,
            },
            SubDispatch {
                goal: "task 2".into(),
                allowed_mcps: vec![],
                success_criteria: vec![],
                correlation_id: "c2".into(),
                label: "2".into(),
                workspace_root: None,
            },
        ];

        let report = orch
            .dispatch_parallel(sub_dispatches, 1)
            .await
            .expect("dispatch_parallel");

        // Both should have run (sequentially)
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(report.summary.contains("task 1"));
        assert!(report.summary.contains("task 2"));
        assert_eq!(report.outcome, Outcome::Succeeded);
    }

    #[tokio::test]
    async fn dispatch_parallel_with_zero_max_concurrent_uses_one() {
        // max_concurrent=0 should be treated as 1 (no panic/deadlock)
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [submit_report_response("only task", "succeeded")],
        ));
        let factory = CallRecordingFactory::default();
        let orch = Orchestrator::new(
            provider,
            factory,
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
            "default",
        );

        let sub_dispatches = vec![SubDispatch {
            goal: "only".into(),
            allowed_mcps: vec![],
            success_criteria: vec![],
            correlation_id: "c1".into(),
            label: "only".into(),
            workspace_root: None,
        }];

        let report = orch
            .dispatch_parallel(sub_dispatches, 0)
            .await
            .expect("dispatch_parallel with max_concurrent=0");

        assert_eq!(report.outcome, Outcome::Succeeded);
        assert!(report.summary.contains("only task"));
    }

    // ------------------------------------------------------------------
    // role journaling: subagent vs. direct execution (deliverable §2)
    // ------------------------------------------------------------------

    /// One fixture, both roles: a delegated subagent call must journal `role: "subagent"` while an
    /// `ExecuteDirect` call still journals `role: "orchestrator"`. A fixture of only subagent calls
    /// would pass an implementation that labels everything `subagent`.
    ///
    /// Exercises both orchestrator arms a cron/vault trigger reaches: the `ExecuteDirect` arm
    /// (871/879) and `dispatch_parallel`'s `tokio::spawn`ed worker (1275) — the spawn case
    /// especially, because the role has to survive the task boundary the constructor-bound provider
    /// exists for.
    #[tokio::test]
    async fn subagent_and_direct_execution_journal_distinct_roles() {
        use liberado_provider::{AgentRole, LatencyEvent, LatencyRecorder, MeteredProvider};

        #[derive(Default)]
        struct CapturingRecorder {
            events: std::sync::Mutex<Vec<LatencyEvent>>,
        }
        impl LatencyRecorder for CapturingRecorder {
            fn record(&self, event: LatencyEvent) {
                self.events.lock().unwrap().push(event);
            }
        }

        let rec = Arc::new(CapturingRecorder::default());
        let direct = MeteredProvider::wrap(
            Arc::new(MockProvider::with_script(
                "mock",
                [submit_report_response("direct done", "succeeded")],
            )),
            AgentRole::Orchestrator,
            rec.clone(),
        );
        let subagent = MeteredProvider::wrap(
            Arc::new(MockProvider::with_script(
                "mock",
                [submit_report_response("delegated done", "succeeded")],
            )),
            AgentRole::Subagent,
            rec.clone(),
        );
        let orch = Orchestrator::new(
            direct,
            CallRecordingFactory::default(),
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
            "default",
        )
        .with_subagent_provider(subagent);

        // Direct execution (the `ExecuteDirect` arm) must keep journaling as `orchestrator`.
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: Vec::new(),
                delivery: Delivery::Summarize,
            },
            confidence: 0.9,
            rationale: "simple".into(),
        };
        let disposition = orch
            .run(
                decision,
                "do it directly",
                "trigger-direct",
                &CapabilitySet::empty(),
            )
            .await
            .expect("direct run");
        assert!(matches!(disposition, Disposition::Reported(_)));

        // Delegated subagent work (the `dispatch_parallel` spawn) must journal as `subagent`.
        orch.dispatch_parallel(
            vec![SubDispatch {
                goal: "delegate".into(),
                allowed_mcps: vec![],
                success_criteria: vec![],
                correlation_id: "delegated-1".into(),
                label: "D".into(),
                workspace_root: None,
            }],
            1,
        )
        .await
        .expect("dispatch_parallel");

        let events = rec.events.lock().unwrap();
        let roles: Vec<&str> = events.iter().map(|e| e.role).collect();
        assert!(
            roles.contains(&"orchestrator"),
            "ExecuteDirect must journal as orchestrator: {roles:?}"
        );
        assert!(
            roles.contains(&"subagent"),
            "delegated subagent work must journal as subagent: {roles:?}"
        );
    }

    /// The **plain `DispatchSubagent` arm** — a chat `delegate`, and the path that produces most of
    /// the journal's subagent records.
    ///
    /// The test above takes its subagent side from `dispatch_parallel`, so this single-dispatch arm
    /// was unguarded: routing it back through the orchestrator's own provider left the whole
    /// workspace green (108/108). That is the R7 shape — a fixture that cannot fail the wrong
    /// implementation, on the busiest of the three subagent paths.
    ///
    /// Also stronger than existence: the recorder sees exactly one call here, so the role is
    /// unambiguously attributable to *this* dispatch rather than to whichever call happened to
    /// contribute it in a mixed list.
    #[tokio::test]
    async fn a_plain_dispatch_subagent_journals_as_subagent() {
        use liberado_provider::{AgentRole, LatencyEvent, LatencyRecorder, MeteredProvider};

        #[derive(Default)]
        struct CapturingRecorder {
            events: std::sync::Mutex<Vec<LatencyEvent>>,
        }
        impl LatencyRecorder for CapturingRecorder {
            fn record(&self, event: LatencyEvent) {
                self.events.lock().unwrap().push(event);
            }
        }

        let rec = Arc::new(CapturingRecorder::default());
        // The orchestrator's own provider is tagged `Orchestrator`; if the dispatch runs on it, the
        // recorded role is wrong and this fails.
        let direct = MeteredProvider::wrap(
            Arc::new(MockProvider::with_script(
                "mock",
                [submit_report_response("wrong provider", "succeeded")],
            )),
            AgentRole::Orchestrator,
            rec.clone(),
        );
        let subagent = MeteredProvider::wrap(
            Arc::new(MockProvider::with_script(
                "mock",
                [submit_report_response("delegated done", "succeeded")],
            )),
            AgentRole::Subagent,
            rec.clone(),
        );
        let orch = Orchestrator::new(
            direct,
            CallRecordingFactory::default(),
            CapabilitySet::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
            "default",
        )
        .with_subagent_provider(subagent);

        let decision = DispatchDecision {
            action: DispatchAction::DispatchSubagent {
                goal: "compare belt drive vs chain drive".into(),
                capabilities: CapabilitySet::empty(),
                allowed_mcps: Vec::new(),
                success_criteria: Vec::new(),
                artifact_target: None,
                model: None,
                correlation_id: "chat-delegate-plain".into(),
                delivery: Delivery::Summarize,
                depth: Depth::Normal,
            },
            confidence: 0.9,
            rationale: "multi-step".into(),
        };
        orch.run(decision, "outer", "trigger-plain", &CapabilitySet::empty())
            .await
            .expect("run");

        let events = rec.events.lock().unwrap();
        assert_eq!(
            events.len(),
            1,
            "precondition: exactly one call, so the role below is attributable to this dispatch"
        );
        assert_eq!(
            events[0].role, "subagent",
            "a plain DispatchSubagent must journal as subagent, not merge into orchestrator"
        );
    }

    #[test]
    fn terminal_summary_failed_outcome_maps_to_failed_terminal_kind() {
        let report = Report {
            outcome: Outcome::Failed,
            summary: "it broke".into(),
            artifacts: vec![],
            new_high_signal_facts: vec![],
            follow_up: None,
            deferred_to_human: false,
            repeat_calls: 0,
        };
        let (kind, summary) = Disposition::Reported(report).terminal_summary();
        assert_eq!(kind, TerminalKind::Failed);
        assert_eq!(summary, "it broke");
    }

    #[test]
    fn terminal_summary_partial_success_prefixes_the_summary() {
        let report = Report {
            outcome: Outcome::PartiallySucceeded,
            summary: "one of two passed".into(),
            artifacts: vec![],
            new_high_signal_facts: vec![],
            follow_up: None,
            deferred_to_human: false,
            repeat_calls: 0,
        };
        let (kind, summary) = Disposition::Reported(report).terminal_summary();
        assert_eq!(kind, TerminalKind::Succeeded);
        assert_eq!(summary, "partially succeeded: one of two passed");
    }

    #[test]
    fn deferred_flag_of_reads_the_atomic() {
        let false_flag = Arc::new(AtomicBool::new(false));
        assert!(!deferred_flag_of(&false_flag));

        let true_flag = Arc::new(AtomicBool::new(true));
        assert!(deferred_flag_of(&true_flag));
    }

    #[test]
    fn no_mcp_runtime_catalog_is_empty() {
        let rt = NoMcpRuntime;
        assert!(rt.catalog().is_empty());
    }

    #[tokio::test]
    async fn no_mcp_runtime_invoke_returns_error() {
        let rt = NoMcpRuntime;
        let call = ToolInvocation::new("id", "some_tool", serde_json::Value::Null);
        let result = rt.invoke(&call).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no MCP"));
    }

    #[test]
    fn vault_delivery_path_accepts_valid() {
        assert_eq!(vault_delivery_path("zone/name.md").unwrap(), "zone/name.md");
        assert_eq!(
            vault_delivery_path("/zone/name.md").unwrap(),
            "zone/name.md"
        );
    }

    #[test]
    fn vault_delivery_path_rejects_bare_filename() {
        assert!(vault_delivery_path("name.md").is_err());
    }

    #[test]
    fn vault_delivery_path_rejects_empty_segments() {
        assert!(vault_delivery_path("zone//name.md").is_err());
    }

    #[test]
    fn looks_like_a_document_accepts_at_threshold() {
        let body = format!("# {}", "x".repeat(MIN_DELIVERED_DOCUMENT_BYTES - 2));
        assert_eq!(body.len(), MIN_DELIVERED_DOCUMENT_BYTES);
        assert!(looks_like_a_document(&body).is_ok());
    }

    #[test]
    fn looks_like_a_document_rejects_below_threshold() {
        let text = "x".repeat(MIN_DELIVERED_DOCUMENT_BYTES - 1);
        assert!(looks_like_a_document(&text).is_err());
    }

    #[test]
    fn looks_like_a_document_accepts_bullets() {
        let body = format!(
            "{}\n- point one\n- point two\n- point three",
            "x".repeat(MIN_DELIVERED_DOCUMENT_BYTES)
        );
        assert!(looks_like_a_document(&body).is_ok());
    }

    #[test]
    fn looks_like_a_document_accepts_numbered_list() {
        let body = format!(
            "{}\n1. first\n1. second\n1. third",
            "x".repeat(MIN_DELIVERED_DOCUMENT_BYTES)
        );
        assert!(looks_like_a_document(&body).is_ok());
    }

    #[test]
    fn execute_direct_building_line_is_emitted_at_info_level() {
        let _guard = TRACING_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        use std::sync::Mutex;
        use tracing::subscriber;
        use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

        #[derive(Default)]
        struct Captured(Arc<Mutex<Vec<(tracing::Level, String)>>>);
        impl<S: tracing::Subscriber> Layer<S> for Captured {
            fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
                struct Msg(String);
                impl tracing::field::Visit for Msg {
                    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                        if f.name() == "message" {
                            self.0 = format!("{v:?}");
                        }
                    }
                }
                let mut m = Msg(String::new());
                event.record(&mut m);
                self.0
                    .lock()
                    .unwrap()
                    .push((*event.metadata().level(), m.0));
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let sub = tracing_subscriber::registry().with(Captured(seen.clone()));

        subscriber::with_default(sub, || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let orch = orchestrator_with_catalog(Vec::new());
                    let decision = DispatchDecision {
                        action: DispatchAction::ExecuteDirect {
                            seed_calls: Vec::new(),
                            relevant_mcps: Vec::new(),
                            delivery: Delivery::Summarize,
                        },
                        confidence: 0.9,
                        rationale: "test".into(),
                    };
                    let _ = orch
                        .run(decision, "goal", "trigger", &CapabilitySet::empty())
                        .await;
                    // The build line is traced by the run's own task; under load the span close /
                    // runtime teardown can deliver the event a moment after `run` returns, and an
                    // immediate read of `seen` raced it (intermittent "build line must be emitted"
                    // in full-suite runs). Poll a short window *inside* the with_default closure,
                    // while the capturing subscriber is still installed; the final assert below
                    // still requires the line (and its level) unconditionally.
                    for _ in 0..100 {
                        if seen
                            .lock()
                            .unwrap()
                            .iter()
                            .any(|(_, m)| m.contains("building execute-direct task"))
                        {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                });
        });

        let events = seen.lock().unwrap();
        let line = events
            .iter()
            .find(|(_, m)| m.contains("building execute-direct task"))
            .expect("the execute-direct build line must be emitted");
        assert_eq!(
            line.0,
            tracing::Level::INFO,
            "A1 exists because the box runs at info; a debug-level line is unobservable there"
        );
    }

    /// The catalog measurement A1 logs: offered MCPs, how many actually appear in the connected
    /// runtime's catalog, and an estimate of the schema's token cost.
    ///
    /// Calls the measurement directly. It used to run a real dispatch and read the values back out
    /// of a `#[cfg(test)]` global that production code wrote to — which raced any other test
    /// touching the same path and failed roughly one run in six with `left: 0` (nothing arrived).
    /// A side channel in production code cannot be made reliable by guarding it; removing it can.
    #[test]
    fn catalog_measurements_counts_offered_surviving_and_schema_size() {
        let offered = vec!["tasks-mcp".to_string(), "email-mcp".to_string()];
        // Two offered, but only one shows up in the connected catalog — the gap A1 exists to see.
        let catalog = vec![
            ToolDef {
                name: "tasks-mcp:add".into(),
                description: "add a task".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolDef {
                name: "tasks-mcp:list".into(),
                description: "list tasks".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        ];

        let (offered_n, surviving, est) = super::catalog_measurements(&offered, &catalog);
        assert_eq!(offered_n, 2, "offered = allowed_mcps.len()");
        assert_eq!(
            surviving, 1,
            "surviving counts distinct MCPs present in the catalog, not tools"
        );
        assert!(est > 0, "schema must have a positive token estimate");

        // Empty catalog: nothing survives, and the estimate must not claim tokens for nothing.
        let (_, none_surviving, empty_est) = super::catalog_measurements(&offered, &[]);
        assert_eq!(none_surviving, 0);
        assert!(
            empty_est < est,
            "an empty catalog must estimate fewer tokens than a populated one"
        );
    }
}
