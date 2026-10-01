//! Liberado-owned switch for timed vault reminders.
//!
//! TurboVault keeps task dates and Full Calendar fields. This table decides whether the
//! minute tick may send them. Default is off.

use liberado_common::{Error, Result};
use serde::{Deserialize, Serialize};

use super::topology::CronSchedule;

/// Job kind the daemon runs once a minute when a schedule names it.
pub(crate) const TICK_JOB: &str = "vault-reminder-tick";

/// Automatic timed reminders read from vault notes.
///
/// `enabled` is the master switch (default off). `tasks` and `events` apply only while
/// that switch is on. A missing table, or a table that sets only `enabled`, keeps the
/// other two at their defaults (on).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultRemindersConfig {
    /// Master switch. The minute tick sends nothing while this is false.
    pub enabled: bool,
    /// Fire remind-marked tasks while the master switch is on.
    pub tasks: bool,
    /// Fire remind-marked calendar notes while the master switch is on.
    pub events: bool,
}

impl Default for VaultRemindersConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tasks: true,
            events: true,
        }
    }
}

/// An enabled `vault-reminder-tick` schedule is a load error while the master switch is off.
///
/// A disabled schedule may still name the job, so an example can show the tick without
/// turning reminders on.
pub(super) fn refuse_tick_while_disabled(schedule: &CronSchedule, enabled: bool) -> Result<()> {
    if enabled || !schedule.enabled {
        return Ok(());
    }
    if schedule.job.as_deref() != Some(TICK_JOB) {
        return Ok(());
    }
    Err(Error::Config(format!(
        "topology.schedules['{}'] job {TICK_JOB} requires [vault_reminders] enabled = true",
        schedule.name
    )))
}

#[cfg(test)]
#[path = "vault_reminders_tests.rs"]
mod vault_reminders_tests;
