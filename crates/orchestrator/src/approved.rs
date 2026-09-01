//! Approved adaptive-goal continuation. Split from `lib.rs` so the signed-goal path can grow
//! without pushing the crate root past its module-health baseline.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use liberado_common::{
    ApprovedGuard, CapabilitySet, Delivery, Outcome, Proposal, ProposedAction, Report,
    WriteProvenance,
};
use liberado_executor::{RiskGatedToolRuntime, Task, ToolRuntime};

use crate::{
    DIRECT_INSTRUCTIONS, NoMcpRuntime, Orchestrator, OrchestratorError, OrchestratorInfra,
};

impl OrchestratorInfra {
    /// Set the direct-worker turn ceiling for every pool.
    pub fn with_direct_max_turns(mut self, max_turns: u32) -> Self {
        self.direct_max_turns = max_turns;
        self
    }

    /// Set the normal acting-subagent turn ceiling for every pool.
    pub fn with_subagent_max_turns(mut self, max_turns: u32) -> Self {
        self.subagent_max_turns = max_turns;
        self
    }

    /// Set the risk-waiver set propagated to every pool's runtime magnitude guard.
    pub fn with_risk_waivers(mut self, waivers: liberado_common::RiskWaiverSet) -> Self {
        self.risk_waivers = waivers;
        self
    }
}

impl Orchestrator {
    pub(crate) async fn execute_approved_other(
        &self,
        proposal: &Proposal,
        action: &ProposedAction,
    ) -> Result<Report, OrchestratorError> {
        match action {
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
            report.deferred_to_human = crate::deferred_flag_of(&deferral);
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

    pub(crate) fn gate_with_approved_guard(
        &self,
        runtime: Box<dyn ToolRuntime>,
        capabilities: CapabilitySet,
        goal_context: impl Into<String>,
        correlation_base: impl Into<String>,
        approved_guard: Option<ApprovedGuard>,
    ) -> (Arc<dyn ToolRuntime>, Arc<AtomicBool>) {
        let deferral_flag = Arc::new(AtomicBool::new(false));
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
        )
        .with_risk_waivers(self.risk_waivers.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use liberado_common::{CapabilityCatalog, CapabilitySet, ProposalSigner, WriteProvenance};
    use liberado_executor::{RuntimeFactory, RuntimeSetupError};
    use liberado_provider::MockProvider;
    use std::sync::Arc;

    struct NoopFactory;

    #[async_trait]
    impl RuntimeFactory for NoopFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            unreachable!("budget test never runs")
        }
    }

    #[test]
    fn infra_applies_configured_direct_normal_and_research_budgets_to_each_pool() {
        let provider = Arc::new(MockProvider::with_script("mock", vec![]));
        let infra = OrchestratorInfra::new(
            provider,
            Arc::new(CapabilityCatalog::new()),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
        )
        .with_direct_max_turns(8)
        .with_subagent_max_turns(20)
        .with_research_max_turns(30);
        let orch = infra.for_pool(NoopFactory, CapabilitySet::empty(), "default");

        assert_eq!(orch.direct_budget.max_turns, 8);
        assert_eq!(orch.subagent_budget.max_turns, 20);
        assert_eq!(orch.research_budget.max_turns, 30);
    }
}
