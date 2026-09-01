//! Turn-budget validation kept off `config.rs` so that file stays at its function baseline.

use liberado_common::{Error, Result};

use super::Config;

impl Config {
    pub(super) fn validate_turn_budgets(&self) -> Result<()> {
        for (name, value) in [
            ("topology.direct_max_turns", self.topology.direct_max_turns),
            (
                "topology.subagent_max_turns",
                self.topology.subagent_max_turns,
            ),
            (
                "topology.research_max_turns",
                self.topology.research_max_turns,
            ),
            (
                "topology.main_agent.max_turns",
                self.topology.main_agent.max_turns,
            ),
        ] {
            if value == Some(0) {
                return Err(Error::Config(format!("{name} must be >= 1")));
            }
        }
        Ok(())
    }
}
