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

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use liberado_common::{
    BlockReason, Capability, CapabilitySet, Consequence, DispatchAction, DispatchDecision,
    McpDescriptor, Outcome, Proposal, ProposalSigner, ProposedAction, Report, SignedProposal,
    ToolCall, WriteClass, WriteProvenance, mcp_of,
};
use liberado_executor::{
    Budget, ExecError, Executor, RiskGatedToolRuntime, RuntimeFactory, RuntimeSetupError, Task,
    ToolRuntime,
};
use liberado_notify::Notifier;
use liberado_provider::{Provider, ToolDef, ToolInvocation};
use liberado_session::TerminalKind;
use thiserror::Error;
use tokio::sync::Semaphore;
use tracing::Instrument;

/// Default `source` recorded in write provenance for orchestrated executions.
pub const DEFAULT_SOURCE: &str = "liberado-executor";

/// Turn budget for an `ExecuteDirect` (kept tight — it is the "few steps clearly suffice" path).
pub const DIRECT_MAX_TURNS: u32 = 4;

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
    factory: Box<dyn RuntimeFactory>,
    /// The ceiling `ExecuteDirect` scopes its runtime to (see `run`'s `ExecuteDirect` arm). The
    /// same `CapabilitySet` the caller's dispatch request was checked against — passing a wider
    /// one here would let a direct execution reach MCPs the guard pre-flight never considered.
    capabilities: CapabilitySet,
    /// `(mcp_name, consequence)` pairs for the runtime-level gate's consequence check (see `gate`).
    consequence_catalog: Vec<(String, Consequence)>,
    /// MCP descriptors (zone declarations) for the runtime-level gate's zone-write-class check.
    zone_catalog: Vec<McpDescriptor>,
    /// `(zone, write_class)` pairs from `Policy.zones` for the same check.
    zone_write_classes: Vec<(String, WriteClass)>,
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
    /// Told about every proposal a runtime-level `gate` downgrade writes — optional, `None` by
    /// default. Best-effort: a notification failure never blocks the write it's reporting on.
    notifier: Option<Arc<dyn Notifier>>,
}

/// The 6 of [`Orchestrator::new`]'s 9 parameters that are the same for every pool a given daemon
/// configures (Decision 18 checkpoint #3) — everything except a pool's own [`RuntimeFactory`]
/// (registries aren't `Clone`/shareable across orchestrators), its `CapabilitySet` ceiling, and its
/// name. `crates/bootstrap/src/lib.rs`'s `configure_daemon` used to build these 6 values once via
/// [`liberado_config::guard_context`] and then re-clone all of them into `Orchestrator::new` at
/// every pool's call site (`docs/roadmap/hygiene-audit-2026-07-05.md`) — building one
/// `OrchestratorInfra` and calling [`for_pool`](Self::for_pool) per pool collapses that back down to
/// naming only what actually differs.
pub struct OrchestratorInfra {
    provider: Arc<dyn Provider>,
    consequence_catalog: Vec<(String, Consequence)>,
    zone_catalog: Vec<McpDescriptor>,
    zone_write_classes: Vec<(String, WriteClass)>,
    proposals_dir: PathBuf,
    signer: ProposalSigner,
}

impl OrchestratorInfra {
    pub fn new(
        provider: Arc<dyn Provider>,
        consequence_catalog: Vec<(String, Consequence)>,
        zone_catalog: Vec<McpDescriptor>,
        zone_write_classes: Vec<(String, WriteClass)>,
        proposals_dir: PathBuf,
        signer: ProposalSigner,
    ) -> Self {
        Self {
            provider,
            consequence_catalog,
            zone_catalog,
            zone_write_classes,
            proposals_dir,
            signer,
        }
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
            factory: Box::new(factory),
            capabilities,
            consequence_catalog: self.consequence_catalog.clone(),
            zone_catalog: self.zone_catalog.clone(),
            zone_write_classes: self.zone_write_classes.clone(),
            proposals_dir: self.proposals_dir.clone(),
            signer: self.signer.clone(),
            pool_name: pool_name.into(),
            source: DEFAULT_SOURCE.to_string(),
            direct_budget: Budget::new(DIRECT_MAX_TURNS),
            subagent_budget: Budget::default(),
            notifier: None,
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
            provider,
            factory: Box::new(factory),
            capabilities,
            consequence_catalog,
            zone_catalog,
            zone_write_classes,
            proposals_dir,
            signer,
            pool_name: pool_name.into(),
            source: DEFAULT_SOURCE.to_string(),
            direct_budget: Budget::new(DIRECT_MAX_TURNS),
            subagent_budget: Budget::default(),
            notifier: None,
        }
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
                } => {
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
                    tracing::debug!(
                        seed_count = seed_calls.len(),
                        allowed_mcps = allowed_mcps.len(),
                        "building execute-direct task"
                    );
                    let task = Task::new(DIRECT_INSTRUCTIONS, goal).with_seed(seed_calls);
                    let report = if allowed_mcps.is_empty() {
                        self.execute(&self.direct_budget, &NoMcpRuntime, task)
                            .await?
                    } else {
                        let provenance =
                            WriteProvenance::agent(self.source.clone(), trigger_correlation);
                        let runtime = self.factory.runtime_for(&allowed_mcps, provenance).await?;
                        let (runtime, deferral) =
                            self.gate(runtime, effective.clone(), goal, trigger_correlation);
                        let mut report = self.execute(&self.direct_budget, &*runtime, task).await?;
                        // If the gate deferred a call to the human out-of-band mid-run, mark it so a
                        // chat surface can drop the redundant reply (Gap 2).
                        report.deferred_to_human = deferred_flag_of(&deferral);
                        report
                    };
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
                    tracing::debug!(
                        subagents = allowed_mcps.len(),
                        criteria = success_criteria.len(),
                        "building subagent task"
                    );
                    let task = Task::new(subagent_instructions(&success_criteria), subgoal);
                    let mut report = self.execute(&self.subagent_budget, &*runtime, task).await?;
                    // Out-of-band deferral mid-run → mark it so a chat surface drops the redundant
                    // reply (Gap 2). This is the primary path for a delegated subagent's permission
                    // request bubbling up through `delegate`.
                    report.deferred_to_human = deferred_flag_of(&deferral);
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
        })
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
        let task = Task::new(subagent_instructions(success_criteria), goal);
        let mut report = self.execute(&self.subagent_budget, &*runtime, task).await?;
        report.deferred_to_human = deferred_flag_of(&deferral);
        tracing::info!(outcome = ?report.outcome, "executed approved subagent proposal");
        Ok(report)
    }

    /// Run multiple subagent dispatches in parallel, each capability-narrowed to the MCPs
    /// its sub-goal actually needs. Results are collected into a single merged Report.
    /// Bounded by `max_concurrent` (from `tuning.dispatch.max_concurrent_subagents`).
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
                .runtime_for(&sub.allowed_mcps, provenance)
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
            deferrals.push(deferral);
            let task = Task::new(subagent_instructions(&sub.success_criteria), sub.goal);
            let budget = self.subagent_budget.clone();
            let provider = self.provider.clone();
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
    /// duplicating exactly this line rather than sharing it (`docs/roadmap/hygiene-audit-2026-07-05.md`).
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
        let deferral_flag = Arc::new(AtomicBool::new(false));
        let mut gated = RiskGatedToolRuntime::new(
            Arc::from(runtime),
            capabilities,
            self.consequence_catalog.clone(),
            self.zone_catalog.clone(),
            self.zone_write_classes.clone(),
            self.proposals_dir.clone(),
            goal_context.into(),
            correlation_base.into(),
            self.signer.clone(),
            self.pool_name.clone(),
        )
        .with_deferral_flag(deferral_flag.clone());
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
///   synthesize `ExecuteMcp` entries from `allowed_mcps`, then intersect with the ceiling.
///   Empty `allowed_mcps` means no MCP narrowing (same sense as empty `relevant_mcps` on
///   `ExecuteDirect`), so the gate is the full ceiling; the runtime still only exposes
///   registered servers.
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
    let requested: CapabilitySet = allowed_mcps
        .iter()
        .map(|name| Capability::ExecuteMcp(name.clone()))
        .collect();
    ceiling.narrow(&requested)
}

/// Build the subagent system prompt, appending its success criteria when present.
fn subagent_instructions(success_criteria: &[String]) -> String {
    if success_criteria.is_empty() {
        return SUBAGENT_PREAMBLE.to_string();
    }
    let criteria = success_criteria
        .iter()
        .map(|c| format!("- {c}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{SUBAGENT_PREAMBLE}\n\nYou are done when:\n{criteria}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_executor::SUBMIT_REPORT_TOOL;
    use liberado_provider::{CompletionResponse, MockProvider};
    use liberado_test_support::CallRecordingFactory;

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
            },
            SubDispatch {
                goal: "do B".into(),
                allowed_mcps: vec!["mcp-b".into()],
                success_criteria: vec![],
                correlation_id: "corr-b".into(),
                label: "B".into(),
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
            },
            SubDispatch {
                goal: "do B".into(),
                allowed_mcps: vec![],
                success_criteria: vec![],
                correlation_id: "corr-b".into(),
                label: "B".into(),
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
            },
            SubDispatch {
                goal: "task 2".into(),
                allowed_mcps: vec![],
                success_criteria: vec![],
                correlation_id: "c2".into(),
                label: "2".into(),
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
        }];

        let report = orch
            .dispatch_parallel(sub_dispatches, 0)
            .await
            .expect("dispatch_parallel with max_concurrent=0");

        assert_eq!(report.outcome, Outcome::Succeeded);
        assert!(report.summary.contains("only task"));
    }
}
