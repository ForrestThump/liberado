//! Exactly one timing form per `[[schedules]]` row, compiled for the cron runtime.
//!
//! [`CronSchedule`] is the row. Structured `days` + `at` become a 7-field expression
//! evaluated in an IANA zone. `cron_expr` stays UTC unless the row also sets `timezone`.

use std::str::FromStr;

use liberado_common::{UserTimezone, compile_cron, parse_civil};
use serde::{Deserialize, Serialize};

fn cron_schedule_enabled_by_default() -> bool {
    true
}

/// A configured cron schedule: wiring only (Decision 14) — the daemon-assembly layer
/// (`liberado-bootstrap`) translates enabled entries into `liberado-cron`'s runtime `Schedule`
/// type.
///
/// Exactly one timing form per row:
///
/// - `cron_expr`: the `cron` crate's 6/7-field syntax (**seconds first**). The fields are
///   **UTC** unless `timezone` is set, in which case they are civil time in that IANA zone.
/// - `days` + `at`: a weekday clock such as `days = ["tue"]`, `at = "20:00"`. It is evaluated
///   in `timezone`, or in `topology.timezone` when `timezone` is omitted, so the clock stays
///   put across DST.
///
/// The daemon still stamps "Local time: …" from `topology.timezone` onto the goal text when a
/// schedule fires (see [`liberado_common::UserTimezone`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronSchedule {
    pub name: String,
    #[serde(default = "cron_schedule_enabled_by_default")]
    pub enabled: bool,
    /// Escape-hatch cron. Empty when `days` and `at` are set.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cron_expr: String,
    /// Weekday names for the structured form: `mon` … `sun`, or the full English name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<String>,
    /// 24-hour clock `HH:MM` or `HH:MM:SS` for the structured form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// IANA zone for this row. Structured rows default to `topology.timezone`.
    /// A `cron_expr` row is UTC when this is omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// The goal text dispatched. Empty when `job` handles the firing without a model.
    #[serde(default)]
    pub goal: String,
    /// Which named pool (Decision 18 checkpoint #3) handles this schedule's firing — `None` routes
    /// to the always-present `"default"` pool (today's behavior, unchanged for anyone not opting
    /// in). If set, must name `"default"` or a declared, enabled `topology.pools` entry
    /// (fail-fast validated).
    #[serde(default)]
    pub pool: Option<String>,
    /// Optional `[[session_profiles]]` hat for this schedule (E7). When set, the reaction session
    /// resolves its grant (and idle budget) from the profile — so a cron that *wants* `AskHuman`
    /// can opt in; crons without a profile keep the pool grant, which should omit `AskHuman` (D-d).
    #[serde(default)]
    pub profile: Option<String>,
    /// Push this schedule's result to the notifier when it finishes. Omitted means yes, which is
    /// the behaviour every schedule had before this existed.
    ///
    /// Set `false` for maintenance schedules that fire often and usually do nothing — hourly with
    /// delivery on is 24 notifications a day for "nothing to report".
    #[serde(default)]
    pub deliver: Option<bool>,
    /// Turn ceiling for this schedule's run. Omitted keeps the dispatch path's default (4 turns
    /// direct, 8 for a subagent) — which the schedule does not choose and cannot see.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// Mechanical job kind. When set, the daemon runs the job and does not start a model
    /// session, except `inbox-if-present`, which dispatches `goal` only when the capture
    /// file has text. Known kinds: `git-snapshot`, `task-ping`, `event-ping`, `habit-ping`,
    /// `inbox-if-present`, `vault-reminder-tick`, `weekly-spending-push`.
    #[serde(default)]
    pub job: Option<String>,
    /// Text a `habit-ping` sends. Required for that kind.
    #[serde(default)]
    pub habit_text: Option<String>,
    /// How many open tasks a `task-ping` lists. Default 10. `event-ping` lists at most 10
    /// events and does not read this field.
    #[serde(default)]
    pub task_limit: Option<u32>,
    /// Vault-relative capture file for `inbox-if-present`. Default `Inbox/Capture.md`.
    #[serde(default)]
    pub capture_path: Option<String>,
    /// Git remote for `git-snapshot`. Default `origin`.
    #[serde(default)]
    pub git_remote: Option<String>,
    /// Branch `git-snapshot` pushes `HEAD` to. Omitted pushes `HEAD`.
    #[serde(default)]
    pub git_branch: Option<String>,
    /// Commit identity for `git-snapshot`. Default `Liberado` / `liberado@localhost`.
    #[serde(default)]
    pub git_user_name: Option<String>,
    #[serde(default)]
    pub git_user_email: Option<String>,
    /// Run a `git-snapshot` once at daemon start when the tree is dirty. Default yes.
    /// Other jobs ignore this.
    #[serde(default)]
    pub run_on_start: Option<bool>,
    /// When `true`, dispatch this schedule's `goal` directly without consulting the
    /// router (classifier) model. The router exists to disambiguate human chat into
    /// Execute / Clarify / Skip and add nothing for a fully-specified cron goal: by
    /// construction, a schedule already names the work, and any router parse failure
    /// degrades to `Clarify`, which fails closed for an unattended cron (no `AskHuman`
    /// capability) — silently doing nothing. Recommended for any schedule whose `goal`
    /// is self-contained and pairs naturally with `profile` (which scopes the grant).
    /// Default `false` (today's behaviour).
    #[serde(default)]
    pub direct: Option<bool>,
}

/// Cron expression plus how its fields are read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleTiming {
    /// Expression the cron runtime evaluates.
    pub expr: String,
    /// `Some` means `expr` fields are civil time in this IANA zone.
    /// `None` means the fields are UTC.
    pub civil_zone: Option<String>,
}

impl CronSchedule {
    /// Compile this row. `topology_timezone` fills in a structured row that omits `timezone`.
    pub fn timing(&self, topology_timezone: &str) -> Result<ScheduleTiming, String> {
        match classify(self)? {
            Form::Cron => cron_timing(self),
            Form::Civil => civil_timing(self, topology_timezone),
        }
    }

    /// Refuse a row that does not name one complete clock.
    ///
    /// This is the form half of [`Self::timing`]. It does not parse `cron_expr`
    /// or `at`, so a create card can fail before the writer takes a backup.
    pub fn require_one_clock(&self) -> Result<(), String> {
        classify(self)?;
        Ok(())
    }
}

enum Form {
    Cron,
    Civil,
}

fn classify(schedule: &CronSchedule) -> Result<Form, String> {
    let cron = !schedule.cron_expr.trim().is_empty();
    let days = !schedule.days.is_empty();
    let at = schedule
        .at
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if cron && (days || at) {
        return Err(exactly_one(&schedule.name));
    }
    if cron {
        return Ok(Form::Cron);
    }
    if days && at {
        return Ok(Form::Civil);
    }
    if days || at {
        return Err(format!(
            "topology.schedules['{}'] structured form requires both days and at",
            schedule.name
        ));
    }
    Err(exactly_one(&schedule.name))
}

fn exactly_one(name: &str) -> String {
    format!("topology.schedules['{name}'] must set exactly one of cron_expr or days and at")
}

fn cron_timing(schedule: &CronSchedule) -> Result<ScheduleTiming, String> {
    let expr = schedule.cron_expr.trim().to_string();
    cron::Schedule::from_str(&expr).map_err(|err| {
        format!(
            "topology.schedules['{}'].cron_expr '{expr}' is invalid: {err}",
            schedule.name
        )
    })?;
    let civil_zone = optional_zone(schedule)?;
    Ok(ScheduleTiming { expr, civil_zone })
}

fn civil_timing(
    schedule: &CronSchedule,
    topology_timezone: &str,
) -> Result<ScheduleTiming, String> {
    let at = schedule.at.as_deref().unwrap_or("");
    let spec = parse_civil(&schedule.days, at)
        .map_err(|err| format!("topology.schedules['{}']: {err}", schedule.name))?;
    let zone = required_zone(schedule, topology_timezone)?;
    let expr = compile_cron(&spec);
    cron::Schedule::from_str(&expr).map_err(|err| {
        format!(
            "topology.schedules['{}'] compiled cron '{expr}' is invalid: {err}",
            schedule.name
        )
    })?;
    Ok(ScheduleTiming {
        expr,
        civil_zone: Some(zone),
    })
}

fn optional_zone(schedule: &CronSchedule) -> Result<Option<String>, String> {
    match schedule.timezone.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => canonical(schedule, raw).map(Some),
        _ => Ok(None),
    }
}

fn required_zone(schedule: &CronSchedule, topology_timezone: &str) -> Result<String, String> {
    if let Some(zone) = optional_zone(schedule)? {
        return Ok(zone);
    }
    let fallback = topology_timezone.trim();
    let name = if fallback.is_empty() {
        liberado_common::DEFAULT_TIMEZONE
    } else {
        fallback
    };
    canonical(schedule, name)
}

fn canonical(schedule: &CronSchedule, raw: &str) -> Result<String, String> {
    match UserTimezone::parse(raw) {
        Ok(zone) => Ok(zone.iana_name().to_string()),
        Err(err) => Err(format!(
            "topology.schedules['{}'].timezone: {err}",
            schedule.name
        )),
    }
}

#[cfg(test)]
#[path = "schedule_form_tests.rs"]
mod tests;
