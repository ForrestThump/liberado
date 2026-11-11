//! `[reminders]` tuning bound checks (dispatch concurrency and reminder channel settings).

use liberado_common::{Error, Result};

use super::Config;

impl Config {
    pub(super) fn validate_tuning_bounds(&self) -> Result<()> {
        self.validate_dispatch_bounds()?;
        self.validate_reminders_bounds()?;
        self.validate_turn_budgets()
    }

    fn validate_dispatch_bounds(&self) -> Result<()> {
        if self.tuning.dispatch.max_concurrent_subagents == 0 {
            return Err(Error::Config(
                "tuning.dispatch.max_concurrent_subagents must be >= 1".into(),
            ));
        }
        if self.tuning.dispatch.max_concurrent_coding_subagents == 0 {
            return Err(Error::Config(
                "tuning.dispatch.max_concurrent_coding_subagents must be >= 1".into(),
            ));
        }
        if self.tuning.concurrency.max_reaction_depth == 0 {
            return Err(Error::Config(
                "tuning.concurrency.max_reaction_depth must be >= 1".into(),
            ));
        }
        Ok(())
    }

    fn validate_reminders_bounds(&self) -> Result<()> {
        if let Some(problem) = self.tuning.reminders.problems().first() {
            return Err(Error::Config(problem.clone()));
        }
        Ok(())
    }
}
