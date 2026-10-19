//! Upcoming fire times for a cron expression.
//!
//! The cron crate evaluates the expression in UTC. This module converts each
//! instant into `tz` with chrono-tz, so the local weekday and the clock follow
//! that zone's DST rules. The weekday is chrono's weekday of the local instant.

use std::str::FromStr;

use chrono::{DateTime, Datelike, Timelike, Utc, Weekday};
use chrono_tz::Tz;

/// One upcoming fire. `weekday` is the local weekday, not the cron field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FireInstant {
    pub utc: DateTime<Utc>,
    pub local: DateTime<Tz>,
    pub weekday: Weekday,
}

impl FireInstant {
    /// English weekday of [`Self::local`] (`Monday`, not `Mon`).
    pub fn weekday_name(&self) -> &'static str {
        weekday_name(self.weekday)
    }

    /// Local reading, for example `Mon Oct 19, 8:00 PM CDT`.
    pub fn display(&self) -> String {
        display_local(&self.local)
    }
}

/// The next `n` fires of `expr` after now, in `tz`.
///
/// An empty `tz` uses `America/Chicago`.
pub fn next_fires(expr: &str, tz: &str, n: usize) -> Result<Vec<FireInstant>, String> {
    next_fires_after(expr, tz, n, Utc::now())
}

/// [`next_fires`] starting strictly after `after`.
pub fn next_fires_after(
    expr: &str,
    tz: &str,
    n: usize,
    after: DateTime<Utc>,
) -> Result<Vec<FireInstant>, String> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let zone = resolve_tz(tz)?;
    let schedule = cron::Schedule::from_str(expr.trim()).map_err(|err| err.to_string())?;
    let mut fires = Vec::with_capacity(n);
    for utc in schedule.after(&after).take(n) {
        fires.push(stamp(utc, zone));
    }
    Ok(fires)
}

fn resolve_tz(tz: &str) -> Result<Tz, String> {
    let name = tz.trim();
    let name = if name.is_empty() {
        "America/Chicago"
    } else {
        name
    };
    name.parse::<Tz>()
        .map_err(|_| format!("unknown timezone '{name}'"))
}

fn stamp(utc: DateTime<Utc>, zone: Tz) -> FireInstant {
    let local = utc.with_timezone(&zone);
    let weekday = local.weekday();
    FireInstant {
        utc,
        local,
        weekday,
    }
}

fn display_local(local: &DateTime<Tz>) -> String {
    let (hour, suffix) = twelve_hour(local.hour());
    format!(
        "{weekday} {month} {day}, {hour}:{minute:02} {suffix} {zone}",
        weekday = short_weekday(local.weekday()),
        month = local.format("%b"),
        day = local.day(),
        minute = local.minute(),
        zone = local.format("%Z"),
    )
}

fn twelve_hour(hour: u32) -> (u32, &'static str) {
    match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    }
}

fn weekday_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

fn short_weekday(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

#[cfg(test)]
#[path = "next_fires_tests.rs"]
mod tests;
