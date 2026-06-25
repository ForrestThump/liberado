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

use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{
    BlockReason, DispatchAction, DispatchDecision, Outcome, Proposal, ProposedAction, Report,
    WriteProvenance, mcp_of,
};
use liberado_executor::{Budget, ExecError, Executor, Task, ToolRuntime};
use liberado_provider::{Provider, ToolInvocation};
use thiserror::Error;
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
    source: String,
    direct_budget: Budget,
    subagent_budget: Budget,
}

impl Orchestrator {
    pub fn new(provider: Arc<dyn Provider>, factory: impl RuntimeFactory + 'static) -> Self {
        Self {
            provider,
            factory: Box::new(factory),
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

                DispatchAction::ExecuteDirect { seed_calls } => {
                    let provenanace =
                        WriteProvenance::agent(self.source.clone(), trigger_correlation);
                    let runtime = self.factory.runtime_for(&[], provenanace).await?;
                    tracing::debug!(
                        seed_count = seed_calls.len(),
                        "building execute-direct task"
                    );
                    let task = Task::new(DIRECT_INSTRUCTIONS, goal).with_seed(seed_calls);
                    let report = self.execute(&self.direct_budget, &*runtime, task).await?;
                    tracing::Span::current().record("disposition", "reported");
                    tracing::info!(outcome = ?report.outcome, "execute-direct completed");
                    Ok(Disposition::Reported(report))
                }

                DispatchAction::DispatchSubagent {
                    goal: subgoal,
                    allowed_mcps,
                    success_criteria,
                    correlation_id,
                    ..
                } => {
                    let provenance = WriteProvenance::agent(self.source.clone(), &correlation_id);
                    let runtime = self.factory.runtime_for(&allowed_mcps, provenance).await?;
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
    use liberado_provider::MockProvider;

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
        let orch = Orchestrator::new(provider, NoopFactory).with_source("custom-source");
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
}
