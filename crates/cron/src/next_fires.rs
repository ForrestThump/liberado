//! Upcoming fire times for a cron expression.
//!
//! The expression is evaluated in UTC. Each instant is then shown in `tz`,
//! so the local weekday and the clock follow that zone's DST rules.

use std::str::FromStr;

use jiff::Zoned;
use jiff::tz::TimeZone;
use liberado_common::{UtcStamp, Weekday};

/// One upcoming fire. `weekday` is the local weekday, not the cron field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FireInstant {
    pub utc: UtcStamp,
    pub local: Zoned,
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
    next_fires_after(expr, tz, n, UtcStamp::now())
}

/// [`next_fires`] starting strictly after `after`.
pub fn next_fires_after(
    expr: &str,
    tz: &str,
    n: usize,
    after: UtcStamp,
) -> Result<Vec<FireInstant>, String> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let zone = resolve_tz(tz)?;
    let schedule = jiff_cron::Schedule::from_str(expr.trim()).map_err(|err| err.to_string())?;
    let start = after.timestamp().to_zoned(TimeZone::UTC);
    let mut fires = Vec::with_capacity(n);
    for zoned in schedule.after(start).take(n) {
        let utc = UtcStamp::from_timestamp(zoned.timestamp());
        fires.push(stamp(utc, zone.clone()));
    }
    Ok(fires)
}

fn resolve_tz(tz: &str) -> Result<TimeZone, String> {
    let name = tz.trim();
    let name = if name.is_empty() {
        "America/Chicago"
    } else {
        name
    };
    TimeZone::get(name).map_err(|_| format!("unknown timezone '{name}'"))
}

pub(crate) fn stamp(utc: UtcStamp, zone: TimeZone) -> FireInstant {
    let local = utc.timestamp().to_zoned(zone);
    let weekday = local.weekday();
    FireInstant {
        utc,
        local,
        weekday,
    }
}

fn display_local(local: &Zoned) -> String {
    let (hour, suffix) = twelve_hour(local.hour());
    format!(
        "{weekday} {month} {day}, {hour}:{minute:02} {suffix} {zone}",
        weekday = short_weekday(local.weekday()),
        month = local.strftime("%b"),
        day = local.day(),
        minute = local.minute(),
        zone = local.strftime("%Z"),
    )
}

fn twelve_hour(hour: i8) -> (i8, &'static str) {
    match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    }
}

fn weekday_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
        Weekday::Sunday => "Sunday",
    }
}

fn short_weekday(day: Weekday) -> &'static str {
    match day {
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
        Weekday::Sunday => "Sun",
    }
}

#[cfg(test)]
#[path = "next_fires_tests.rs"]
mod tests;
