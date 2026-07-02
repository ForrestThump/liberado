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

use async_trait::async_trait;
use liberado_common::{
    BlockReason, CapabilitySet, Consequence, DispatchAction, DispatchDecision, Outcome, Proposal,
    ProposedAction, Report, WriteProvenance, mcp_of,
};
use liberado_executor::{Budget, ExecError, Executor, RiskGatedToolRuntime, Task, ToolRuntime};
use liberado_provider::{Provider, ToolDef, ToolInvocation};
use thiserror::Error;
use tokio::sync::Semaphore;
use tracing::Instrument;

/// Default `source` recorded in write provenance for orchestrated executions.
pub const DEFAULT_SOURCE: &str = "liberado-executor";

/// Turn budget for an `ExecuteDirect` (kept tight — it is the "few steps clearly suffice" path).
pub const DIRECT_MAX_TURNS: u32 = 4;

const DIRECT_INSTRUCTIONS: &str = "\
You are Liberado's executor. Accomplish the goal using the available tools, taking as few steps as \
possible. When the goal is done, call `submit_report` with a concise, high-signal result. Do not \
ask the user anything; if you cannot proceed, submit a report explaining why.";

const SUBAGENT_PREAMBLE: &str = "\
You are a narrowly-scoped Liberado subagent. Use only the tools you have been given to accomplish \
the goal, then call `submit_report` with the result. Do not exceed your goal.";

/// What an orchestrated decision resolved to.
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
    Propose(Proposal),
}

/// Failure building a [`ToolRuntime`] for an execution (connection/handshake/etc.).
#[derive(Debug, Error)]
#[error("{0}")]
pub struct RuntimeSetupError(pub String);

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

/// How the orchestrator obtains a [`ToolRuntime`] for an execution: given the MCPs the execution is
/// allowed to see and the provenance every call should carry, return a connected runtime. The real
/// implementation (turbomcp-backed) lives in the MCP layer; tests inject a mock.
#[async_trait]
pub trait RuntimeFactory: Send + Sync {
    async fn runtime_for(
        &self,
        allowed_mcps: &[String],
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError>;
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
    /// Base directory for proposal files a runtime-level downgrade writes (see `gate`).
    proposals_dir: PathBuf,
    source: String,
    direct_budget: Budget,
    subagent_budget: Budget,
}

impl Orchestrator {
    pub fn new(
        provider: Arc<dyn Provider>,
        factory: impl RuntimeFactory + 'static,
        capabilities: CapabilitySet,
        consequence_catalog: Vec<(String, Consequence)>,
        proposals_dir: PathBuf,
    ) -> Self {
        Self {
            provider,
            factory: Box::new(factory),
            capabilities,
            consequence_catalog,
            proposals_dir,
            source: DEFAULT_SOURCE.to_string(),
            direct_budget: Budget::new(DIRECT_MAX_TURNS),
            subagent_budget: Budget::default(),
        }
    }

    /// Override the provenance `source` recorded for executions (e.g. a per-deployment id).
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Execute `decision`. `goal` is the goal an `ExecuteDirect` should accomplish (a
    /// `DispatchSubagent` carries its own restated goal). `trigger_correlation` is the id of the
    /// event that prompted this decision — the provenance correlation an `ExecuteDirect` adopts.
    pub async fn run(
        &self,
        decision: DispatchDecision,
        goal: &str,
        trigger_correlation: &str,
    ) -> Result<Disposition, OrchestratorError> {
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
                    let proposal = Proposal::pending(
                        trigger_correlation,
                        trigger_correlation,
                        self.source.clone(),
                        proposed_action,
                        rationale,
                    );
                    tracing::Span::current().record("disposition", "proposed");
                    tracing::info!(proposal_id = %proposal.id, "dispatch resulted in a proposal");
                    Ok(Disposition::Propose(proposal))
                }

                DispatchAction::ExecuteDirect {
                    seed_calls,
                    relevant_mcps,
                } => {
                    // Scope to exactly the MCPs `self.capabilities` grants — an empty allow-list
                    // means "every registered MCP" to `RuntimeFactory`/`ScopedRuntime` (the wrong
                    // sense here, same reason `ChatSessions` special-cases it for its own scoping),
                    // which would let an adaptive (non-seed) tool call reach any registered MCP
                    // regardless of what the guard pre-flight actually checked the goal against.
                    let granted: Vec<String> = self.capabilities.granted_mcps();
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
                        self.execute(&self.direct_budget, &NoMcpRuntime, task).await?
                    } else {
                        let provenanace =
                            WriteProvenance::agent(self.source.clone(), trigger_correlation);
                        let runtime = self.factory.runtime_for(&allowed_mcps, provenanace).await?;
                        let runtime =
                            self.gate(runtime, self.capabilities.clone(), goal, trigger_correlation);
                        self.execute(&self.direct_budget, &*runtime, task).await?
                    };
                    tracing::Span::current().record("disposition", "reported");
                    tracing::info!(outcome = ?report.outcome, "execute-direct completed");
                    Ok(Disposition::Reported(report))
                }

                DispatchAction::DispatchSubagent {
                    goal: subgoal,
                    capabilities,
                    allowed_mcps,
                    success_criteria,
                    correlation_id,
                    ..
                } => {
                    let provenance = WriteProvenance::agent(self.source.clone(), &correlation_id);
                    let runtime = self.factory.runtime_for(&allowed_mcps, provenance).await?;
                    // Gate with the ceiling narrowed by the decision's own capabilities (Decision 4:
                    // authority can only shrink down a delegation chain) — belt and suspenders, same
                    // as `ExecuteDirect` re-intersecting `relevant_mcps` against its granted ceiling.
                    let gate_capabilities = self.capabilities.narrow(&capabilities);
                    let runtime = self.gate(
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
                    let report = self.execute(&self.subagent_budget, &*runtime, task).await?;
                    tracing::Span::current().record("disposition", "reported");
                    tracing::info!(outcome = ?report.outcome, "subagent dispatch completed");
                    Ok(Disposition::Reported(report))
                }
            }
        }
        .instrument(span)
        .await
    }

    /// Execute an APPROVED proposal's action directly — approval is the authorization, so this
    /// bypasses the dispatcher/guards entirely (re-dispatching would just re-trigger the consequence
    /// guard and re-propose). It runs exactly the approved calls, in order, via a runtime scoped to
    /// the MCPs they touch, with the proposal's correlation id as provenance.
    pub async fn execute_approved(&self, proposal: &Proposal) -> Result<Report, OrchestratorError> {
        let span = tracing::info_span!(
            "execute_approved",
            proposal_id = %proposal.id,
            correlation = %proposal.correlation_id,
            source = %self.source,
        );
        async {
            let ProposedAction::ToolCalls(calls) = &proposal.proposed_action else {
                // External/VaultWrite/Other aren't produced by v1 emit; refuse defensively rather
                // than error so the daemon can mark the proposal done and not retry forever.
                tracing::warn!(
                    action = ?proposal.proposed_action,
                    "approved proposal action is not executable in v1"
                );
                return Ok(Report {
                    outcome: Outcome::Failed,
                    summary: "proposed action type is not executable in v1".into(),
                    artifacts: Vec::new(),
                    new_high_signal_facts: Vec::new(),
                    follow_up: None,
                });
            };

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
                let inv =
                    ToolInvocation::new(format!("approved-{i}"), &call.tool, call.args.clone());
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
                follow_up: None,
            })
        }
        .instrument(span)
        .await
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

        for sub in sub_dispatches {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let provenance =
                WriteProvenance::agent(self.source.clone(), &sub.correlation_id);
            let runtime = self.factory.runtime_for(&sub.allowed_mcps, provenance).await?;
            // No per-sub-dispatch CapabilitySet exists on `SubDispatch` today (only `allowed_mcps`),
            // so gate with the orchestrator-level ceiling — the same one `ExecuteDirect` uses.
            let runtime = self.gate(
                runtime,
                self.capabilities.clone(),
                sub.goal.as_str(),
                sub.correlation_id.as_str(),
            );
            let task = Task::new(subagent_instructions(&sub.success_criteria), sub.goal);
            let budget = self.subagent_budget;
            let provider = self.provider.clone();
            let label = sub.label.clone();

            let handle = tokio::spawn(async move {
                let result = Executor::new(provider, budget).execute(&*runtime, task).await;
                drop(permit);
                (label, result)
            });
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
        })
    }

    async fn execute(
        &self,
        budget: &Budget,
        runtime: &dyn ToolRuntime,
        task: Task,
    ) -> Result<Report, ExecError> {
        Executor::new(self.provider.clone(), *budget)
            .execute(runtime, task)
            .await
    }

    /// Wrap a connected runtime in the same runtime-level safety net chat's own tool loop already
    /// uses (`RiskGatedToolRuntime`), so the executor's *adaptive* (non-seed) tool calls get the
    /// same capability/consequence/magnitude checking the dispatcher's pre-flight guard only ever
    /// applied to the decision's seed call. Deliberately not used by `execute_approved` — approval
    /// is already the authorization there; re-gating it would re-downgrade an approved call into a
    /// new proposal.
    fn gate(
        &self,
        runtime: Box<dyn ToolRuntime>,
        capabilities: CapabilitySet,
        goal_context: impl Into<String>,
        correlation_base: impl Into<String>,
    ) -> Arc<dyn ToolRuntime> {
        Arc::new(RiskGatedToolRuntime::new(
            Arc::from(runtime),
            capabilities,
            self.consequence_catalog.clone(),
            self.proposals_dir.clone(),
            goal_context.into(),
            correlation_base.into(),
        ))
    }
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
    use std::sync::Mutex;

    use liberado_executor::SUBMIT_REPORT_TOOL;
    use liberado_provider::{CompletionResponse, MockProvider, ToolDef};
    use super::*;

    #[test]
    fn subagent_instructions_with_criteria() {
        let criteria = vec!["find the answer".into(), "write it down".into()];
        let result = subagent_instructions(&criteria);
        assert!(result.contains("find the answer"));
        assert!(result.contains("write it down"));
        assert!(result.contains(SUBAGENT_PREAMBLE));
    }

    #[test]
    fn subagent_instructions_empty_returns_preamble() {
        let result = subagent_instructions(&[]);
        assert_eq!(result, SUBAGENT_PREAMBLE);
    }

    #[test]
    fn with_source_overrides_default() {
        let provider = Arc::new(MockProvider::with_script("mock", vec![]));
        let orch = Orchestrator::new(
            provider,
            NoopFactory,
            CapabilitySet::empty(),
            Vec::new(),
            std::env::temp_dir(),
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

    type Calls = Arc<Mutex<Vec<(Vec<String>, WriteProvenance)>>>;

    #[derive(Clone, Default)]
    struct RecordingFactory {
        calls: Calls,
    }

    #[async_trait]
    impl RuntimeFactory for RecordingFactory {
        async fn runtime_for(
            &self,
            allowed_mcps: &[String],
            provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            self.calls
                .lock()
                .unwrap()
                .push((allowed_mcps.to_vec(), provenance));
            Ok(Box::new(MockRuntime))
        }
    }

    struct MockRuntime;

    #[async_trait]
    impl ToolRuntime for MockRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".to_string())
        }
    }

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
        let factory = RecordingFactory::default();
        let calls = factory.calls.clone();
        let orch = Orchestrator::new(
            provider,
            factory,
            CapabilitySet::empty(),
            Vec::new(),
            std::env::temp_dir(),
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
        let factory = RecordingFactory::default();
        let orch = Orchestrator::new(
            provider,
            factory,
            CapabilitySet::empty(),
            Vec::new(),
            std::env::temp_dir(),
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
        assert_eq!(
            report.new_high_signal_facts,
            vec!["fact A", "fact B"]
        );
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
        let factory = RecordingFactory::default();
        let calls = factory.calls.clone();
        let orch = Orchestrator::new(
            provider,
            factory,
            CapabilitySet::empty(),
            Vec::new(),
            std::env::temp_dir(),
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
        let factory = RecordingFactory::default();
        let orch = Orchestrator::new(
            provider,
            factory,
            CapabilitySet::empty(),
            Vec::new(),
            std::env::temp_dir(),
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
