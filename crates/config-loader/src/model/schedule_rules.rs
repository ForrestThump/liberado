//! Per-schedule load checks. Split out of `config.rs` so that file does not grow.

use liberado_common::{Error, Result};

use super::config::Config;
use super::topology::CronSchedule;
use super::vault_reminders::{refuse_gated_job, refuse_tick_while_disabled};
use super::weekly_spending::WEEKLY_SPENDING_JOB;

pub(super) fn validate_schedules(config: &Config) -> Result<()> {
    let mut seen_schedule_names = std::collections::HashSet::new();
    for schedule in &config.topology.schedules {
        validate_one_schedule(
            schedule,
            &mut seen_schedule_names,
            config.topology.vault_reminders.enabled,
            config.topology.weekly_spending.enabled,
            &config.topology.timezone,
        )?;
    }
    super::weekly_spending::validate_section(&config.topology)?;
    Ok(())
}

/// Per-schedule validation: name uniqueness, cron parse, and the job-kind checks.
///
/// `spending_enabled` is an argument only. It is not a branch in this function.
///
/// Cyclomatic: 1 (base) + 1 (insert) + 1 (cron) + 1 (job Some) + 1 (KINDS)
///             + 2 (habit &&) + 2 (inbox &&) + 1 (gated `?`) = 9.
fn validate_one_schedule<'a>(
    schedule: &'a CronSchedule,
    seen: &mut std::collections::HashSet<&'a str>,
    reminders_enabled: bool,
    spending_enabled: bool,
    topology_tz: &str,
) -> Result<()> {
    if !seen.insert(&schedule.name) {
        return Err(Error::Config(format!(
            "topology.schedules has a duplicate name '{}'",
            schedule.name
        )));
    }
    if let Err(detail) = schedule.timing(topology_tz) {
        return Err(Error::Config(detail));
    }
    let Some(job) = &schedule.job else {
        return Ok(());
    };
    const KINDS: &[&str] = &[
        "git-snapshot",
        "task-ping",
        "event-ping",
        "habit-ping",
        "inbox-if-present",
        "vault-reminder-tick",
        "weekly-spending-push",
    ];
    if !KINDS.contains(&job.as_str()) {
        return Err(Error::Config(format!(
            "topology.schedules['{}'].job '{job}' is not one of {}",
            schedule.name,
            KINDS.join(", ")
        )));
    }
    if job == "habit-ping"
        && schedule
            .habit_text
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        return Err(Error::Config(format!(
            "topology.schedules['{}'] habit-ping requires habit_text",
            schedule.name
        )));
    }
    if job == "inbox-if-present" && schedule.goal.trim().is_empty() {
        return Err(Error::Config(format!(
            "topology.schedules['{}'] inbox-if-present requires goal",
            schedule.name
        )));
    }
    refuse_gated_schedules(schedule, reminders_enabled, spending_enabled)?;
    Ok(())
}

fn refuse_gated_schedules(
    schedule: &CronSchedule,
    reminders_enabled: bool,
    spending_enabled: bool,
) -> Result<()> {
    refuse_tick_while_disabled(schedule, reminders_enabled)?;
    refuse_gated_job(
        schedule,
        spending_enabled,
        WEEKLY_SPENDING_JOB,
        "weekly_spending",
    )?;
    Ok(())
}
