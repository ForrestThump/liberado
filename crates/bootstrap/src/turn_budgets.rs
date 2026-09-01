//! Topology turn-ceiling wiring for [`OrchestratorInfra`].

use liberado_config::Config;
use liberado_orchestrator::OrchestratorInfra;

pub(crate) fn apply_turn_budgets(
    mut infra: OrchestratorInfra,
    config: &Config,
) -> OrchestratorInfra {
    if let Some(max_turns) = config.topology.direct_max_turns {
        infra = infra.with_direct_max_turns(max_turns);
    }
    if let Some(max_turns) = config.topology.subagent_max_turns {
        infra = infra.with_subagent_max_turns(max_turns);
    }
    if let Some(max_turns) = config.topology.research_max_turns {
        infra = infra.with_research_max_turns(max_turns);
    }
    infra
}
