//! Face-agent turn budget derived from topology.

use liberado_executor::Budget;

pub(crate) fn main_agent_budget(config: &liberado_config::MainAgentConfig) -> Budget {
    Budget::new(
        config
            .max_turns
            .unwrap_or(liberado_executor::DEFAULT_MAX_TURNS),
    )
}
