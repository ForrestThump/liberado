//! Interval and clock phrases for a parsed cron expression.
//!
//! Split from `english.rs` so that file stays inside the module-health boundaries.

use chrono::{DateTime, Datelike, Duration, Timelike, Utc, Weekday};
use liberado_common::UserTimezone;

use super::parse::{Atom, Parsed};

struct LocalStamp {
    hour: u32,
    minute: u32,
    weekday: Weekday,
    day: u32,
}

pub(super) fn interval_text(spec: &Parsed) -> Option<String> {
    if !calendar_open(spec) {
        return None;
    }
    every_second(spec)
        .or_else(|| every_minute(spec))
        .or_else(|| every_hour(spec))
}

fn calendar_open(spec: &Parsed) -> bool {
    spec.month_any && spec.year_any && spec.dom.is_any() && dow_is_any(&spec.dow)
}

fn every_second(spec: &Parsed) -> Option<String> {
    if !spec.minute.is_any() || !spec.hour.is_any() {
        return None;
    }
    match &spec.second {
        Atom::Any => Some("Every second".to_string()),
        Atom::Step { start: 0, step } if divides(*step, 60) => {
            Some(every_n(*step, "second", "seconds"))
        }
        _ => None,
    }
}

fn every_minute(spec: &Parsed) -> Option<String> {
    if !is_zero(&spec.second) || !spec.hour.is_any() {
        return None;
    }
    match &spec.minute {
        Atom::Any => Some("Every minute".to_string()),
        Atom::Step { start: 0, step } if divides(*step, 60) => {
            Some(every_n(*step, "minute", "minutes"))
        }
        _ => None,
    }
}

fn every_hour(spec: &Parsed) -> Option<String> {
    if !is_zero(&spec.second) || !is_zero(&spec.minute) {
        return None;
    }
    match &spec.hour {
        Atom::Any => Some("Every hour".to_string()),
        Atom::Step { start: 0, step } if divides(*step, 24) => {
            Some(every_n(*step, "hour", "hours"))
        }
        _ => None,
    }
}

fn divides(step: u32, span: u32) -> bool {
    step > 0 && step < span && span.is_multiple_of(step)
}

fn every_n(step: u32, one: &str, many: &str) -> String {
    if step == 1 {
        format!("Every {one}")
    } else {
        format!("Every {step} {many}")
    }
}

pub(super) fn clock_text(spec: &Parsed, timezone: &str, at: DateTime<Utc>) -> Option<String> {
    if !spec.month_any || !spec.year_any {
        return None;
    }
    let (hour, minute) = clock_parts(spec)?;
    let zone = UserTimezone::parse(timezone).ok()?;
    day_phrase(spec, hour, minute, &zone, at)
}

fn clock_parts(spec: &Parsed) -> Option<(u32, u32)> {
    let second = single(&spec.second)?;
    if second != 0 {
        return None;
    }
    Some((single(&spec.hour)?, single(&spec.minute)?))
}

fn day_phrase(
    spec: &Parsed,
    hour: u32,
    minute: u32,
    zone: &UserTimezone,
    at: DateTime<Utc>,
) -> Option<String> {
    if dow_is_any(&spec.dow) && spec.dom.is_any() {
        return daily_phrase(hour, minute, zone, at);
    }
    if spec.dom.is_any() && is_weekdays(&spec.dow) {
        return weekdays_phrase(hour, minute, zone, at);
    }
    if spec.dom.is_any()
        && let Some(dow) = single(&spec.dow)
    {
        return one_weekday_phrase(dow, hour, minute, zone, at);
    }
    if dow_is_any(&spec.dow)
        && let Some(dom) = single(&spec.dom)
    {
        return dom_phrase(dom, hour, minute, zone, at);
    }
    None
}

fn daily_phrase(hour: u32, minute: u32, zone: &UserTimezone, at: DateTime<Utc>) -> Option<String> {
    let local = local_stamp(zone, at, hour, minute, None, None)?;
    Some(format!(
        "Every day at {}",
        format_time(local.hour, local.minute)?
    ))
}

fn weekdays_phrase(
    hour: u32,
    minute: u32,
    zone: &UserTimezone,
    at: DateTime<Utc>,
) -> Option<String> {
    let days = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
    ];
    let mut local_days = Vec::with_capacity(days.len());
    let mut clock = None;
    for day in days {
        let local = local_stamp(zone, at, hour, minute, Some(day), None)?;
        let this = (local.hour, local.minute);
        if let Some(previous) = clock {
            if previous != this {
                return None;
            }
        } else {
            clock = Some(this);
        }
        local_days.push(local.weekday);
    }
    if local_days != days {
        return None;
    }
    let (local_hour, local_minute) = clock?;
    Some(format!(
        "Weekdays at {}",
        format_time(local_hour, local_minute)?
    ))
}

fn one_weekday_phrase(
    dow: u32,
    hour: u32,
    minute: u32,
    zone: &UserTimezone,
    at: DateTime<Utc>,
) -> Option<String> {
    let utc_day = weekday_from_cron(dow)?;
    let local = local_stamp(zone, at, hour, minute, Some(utc_day), None)?;
    Some(format!(
        "Every {} at {}",
        weekday_name(local.weekday),
        format_time(local.hour, local.minute)?
    ))
}

fn dom_phrase(
    dom: u32,
    hour: u32,
    minute: u32,
    zone: &UserTimezone,
    at: DateTime<Utc>,
) -> Option<String> {
    let local = local_stamp(zone, at, hour, minute, None, Some(dom))?;
    if local.day != dom {
        return None;
    }
    Some(format!(
        "On the {} of every month at {}",
        ordinal(dom),
        format_time(local.hour, local.minute)?
    ))
}

fn local_stamp(
    zone: &UserTimezone,
    at: DateTime<Utc>,
    hour: u32,
    minute: u32,
    utc_dow: Option<Weekday>,
    utc_dom: Option<u32>,
) -> Option<LocalStamp> {
    let instant = matching_utc(at, hour, minute, utc_dow, utc_dom)?;
    let local = zone.at(instant);
    Some(LocalStamp {
        hour: local.hour(),
        minute: local.minute(),
        weekday: local.weekday(),
        day: local.day(),
    })
}

fn matching_utc(
    at: DateTime<Utc>,
    hour: u32,
    minute: u32,
    utc_dow: Option<Weekday>,
    utc_dom: Option<u32>,
) -> Option<DateTime<Utc>> {
    let start = at.date_naive();
    (0..370).find_map(|offset| {
        let date = start.checked_add_signed(Duration::days(offset))?;
        if utc_dow.is_some_and(|day| date.weekday() != day) {
            return None;
        }
        if utc_dom.is_some_and(|dom| date.day() != dom) {
            return None;
        }
        date.and_hms_opt(hour, minute, 0).map(|time| time.and_utc())
    })
}

fn format_time(hour: u32, minute: u32) -> Option<String> {
    let (clock, suffix) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        13..=23 => (hour - 12, "PM"),
        _ => return None,
    };
    Some(format!("{clock}:{minute:02} {suffix}"))
}

pub(super) fn ordinal(day: u32) -> String {
    let suffix = match day % 100 {
        11..=13 => "th",
        _ => match day % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        },
    };
    format!("{day}{suffix}")
}

fn weekday_from_cron(dow: u32) -> Option<Weekday> {
    match dow {
        1 => Some(Weekday::Sun),
        2 => Some(Weekday::Mon),
        3 => Some(Weekday::Tue),
        4 => Some(Weekday::Wed),
        5 => Some(Weekday::Thu),
        6 => Some(Weekday::Fri),
        7 => Some(Weekday::Sat),
        _ => None,
    }
}

fn weekday_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Sun => "Sunday",
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
    }
}

fn dow_is_any(atom: &Atom) -> bool {
    match atom {
        Atom::Any => true,
        Atom::Range { start: 1, end: 7 } => true,
        Atom::List(values) => values.as_slice() == [1, 2, 3, 4, 5, 6, 7],
        _ => false,
    }
}

fn is_weekdays(atom: &Atom) -> bool {
    match atom {
        Atom::Range { start: 2, end: 6 } => true,
        Atom::List(values) => values.as_slice() == [2, 3, 4, 5, 6],
        _ => false,
    }
}

fn is_zero(atom: &Atom) -> bool {
    matches!(atom, Atom::Value(0))
}

fn single(atom: &Atom) -> Option<u32> {
    match atom {
        Atom::Value(value) => Some(*value),
        Atom::List(values) if values.len() == 1 => Some(values[0]),
        Atom::Range { start, end } if start == end => Some(*start),
        _ => None,
    }
}
