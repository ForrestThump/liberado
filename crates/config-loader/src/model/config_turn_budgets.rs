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
        self.validate_profile_turn_budgets()
    }

    fn validate_profile_turn_budgets(&self) -> Result<()> {
        for profile in &self.topology.session_profiles {
            if profile.max_turns == Some(0) {
                return Err(Error::Config(format!(
                    "topology.session_profiles['{}'].max_turns must be >= 1",
                    profile.name
                )));
            }
        }
        Ok(())
    }
}
