//! Exactly one timing form per `[[schedules]]` row, compiled for the cron runtime.
//!
//! Structured `days` + `at` become a 7-field expression evaluated in an IANA zone.
//! `cron_expr` stays UTC unless the row also sets `timezone`.

use std::str::FromStr;

use liberado_common::{UserTimezone, compile_cron, parse_civil};

use super::topology::CronSchedule;

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
