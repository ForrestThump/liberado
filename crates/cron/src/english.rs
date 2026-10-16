//! Plain-English reading of a cron expression.
//!
//! Liberado stores 6- or 7-field expressions, seconds first, in UTC (the `cron` crate:
//! Sunday is 1). A 5-field string is Unix cron, minute first, with seconds taken as 0
//! (Sunday is 0 or 7). Clock times are converted into `timezone` at `at`. An expression
//! this module cannot describe exactly is left unchanged.

use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, Timelike, Utc, Weekday};
use liberado_common::UserTimezone;

/// Read `expr` in `timezone` as of now. Odd or unsupported expressions return `expr`.
pub fn cron_to_english(expr: &str, timezone: &str) -> String {
    cron_to_english_at(expr, timezone, Utc::now())
}

/// [`cron_to_english`] at a fixed UTC instant, so tests can pin the offset.
pub fn cron_to_english_at(expr: &str, timezone: &str, at: DateTime<Utc>) -> String {
    describe(expr.trim(), timezone.trim(), at).unwrap_or_else(|| expr.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Atom {
    Any,
    Value(u32),
    Step { start: u32, step: u32 },
    Range { start: u32, end: u32 },
    List(Vec<u32>),
}

#[derive(Clone, Copy)]
enum FieldKind {
    Second,
    Minute,
    Hour,
    Dom,
    Dow { five: bool },
}

struct Parsed {
    second: Atom,
    minute: Atom,
    hour: Atom,
    dom: Atom,
    month_any: bool,
    dow: Atom,
    year_any: bool,
}

struct LocalStamp {
    hour: u32,
    minute: u32,
    weekday: Weekday,
    day: u32,
}

fn describe(raw: &str, timezone: &str, at: DateTime<Utc>) -> Option<String> {
    let (spec, five_field) = parse_expression(raw)?;
    if !five_field && cron::Schedule::from_str(raw).is_err() {
        return None;
    }
    if let Some(text) = interval_text(&spec) {
        return Some(text);
    }
    clock_text(&spec, timezone, at)
}

fn parse_expression(raw: &str) -> Option<(Parsed, bool)> {
    if let Some(expanded) = expand_shorthand(raw) {
        let tokens: Vec<&str> = expanded.split_whitespace().collect();
        return Some((parse_tokens(&tokens, false)?, false));
    }
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    match tokens.len() {
        5 => {
            let mut fields = Vec::with_capacity(6);
            fields.push("0");
            fields.extend(tokens);
            Some((parse_tokens(&fields, true)?, true))
        }
        6 => Some((parse_tokens(&tokens, false)?, false)),
        7 => Some((parse_tokens(&tokens, false)?, false)),
        _ => None,
    }
}

fn expand_shorthand(raw: &str) -> Option<&'static str> {
    match raw {
        "@yearly" => Some("0 0 0 1 1 * *"),
        "@monthly" => Some("0 0 0 1 * * *"),
        "@weekly" => Some("0 0 0 * * 1 *"),
        "@daily" => Some("0 0 0 * * * *"),
        "@hourly" => Some("0 0 * * * * *"),
        _ => None,
    }
}

fn parse_tokens(tokens: &[&str], five: bool) -> Option<Parsed> {
    let year_any = match tokens.len() {
        6 => true,
        7 => tokens[6] == "*" || tokens[6] == "?",
        _ => return None,
    };
    Some(Parsed {
        second: parse_atom(tokens[0], FieldKind::Second)?,
        minute: parse_atom(tokens[1], FieldKind::Minute)?,
        hour: parse_atom(tokens[2], FieldKind::Hour)?,
        dom: parse_atom(tokens[3], FieldKind::Dom)?,
        month_any: tokens[4] == "*" || tokens[4] == "?",
        dow: parse_atom(tokens[5], FieldKind::Dow { five })?,
        year_any,
    })
}

fn parse_atom(raw: &str, kind: FieldKind) -> Option<Atom> {
    if raw == "*" || raw == "?" {
        return Some(Atom::Any);
    }
    if let Some((start, step)) = raw.split_once('/') {
        if start.contains('/') {
            return None;
        }
        let start = if start == "*" {
            0
        } else {
            resolve_token(start, kind)?
        };
        let step = start_step(step)?;
        return Some(Atom::Step { start, step });
    }
    if raw.contains(',') {
        return parse_list(raw, kind);
    }
    if let Some((left, right)) = raw.split_once('-') {
        let start = resolve_token(left, kind)?;
        let end = resolve_token(right, kind)?;
        if start > end {
            return None;
        }
        return Some(Atom::Range { start, end });
    }
    Some(Atom::Value(resolve_token(raw, kind)?))
}

fn start_step(raw: &str) -> Option<u32> {
    let step = raw.parse::<u32>().ok()?;
    (step > 0).then_some(step)
}

fn parse_list(raw: &str, kind: FieldKind) -> Option<Atom> {
    let mut values = Vec::new();
    for part in raw.split(',') {
        if part.is_empty() || part.contains('-') || part.contains('/') || part.contains('*') {
            return None;
        }
        values.push(resolve_token(part, kind)?);
    }
    if values.is_empty() || values.len() > 31 {
        return None;
    }
    values.sort_unstable();
    values.dedup();
    Some(Atom::List(values))
}

fn resolve_token(token: &str, kind: FieldKind) -> Option<u32> {
    if let Ok(number) = token.parse::<u32>() {
        return match kind {
            FieldKind::Second | FieldKind::Minute => in_range(number, 0, 59),
            FieldKind::Hour => in_range(number, 0, 23),
            FieldKind::Dom => in_range(number, 1, 31),
            FieldKind::Dow { five: false } => in_range(number, 1, 7),
            FieldKind::Dow { five: true } => five_field_dow(number),
        };
    }
    match kind {
        FieldKind::Dow { .. } => dow_name(token),
        _ => None,
    }
}

fn in_range(number: u32, min: u32, max: u32) -> Option<u32> {
    (min..=max).contains(&number).then_some(number)
}

fn five_field_dow(number: u32) -> Option<u32> {
    match number {
        0 | 7 => Some(1),
        1..=6 => Some(number + 1),
        _ => None,
    }
}

fn dow_name(token: &str) -> Option<u32> {
    match token.to_ascii_lowercase().as_str() {
        "sun" | "sunday" => Some(1),
        "mon" | "monday" => Some(2),
        "tue" | "tues" | "tuesday" => Some(3),
        "wed" | "wednesday" => Some(4),
        "thu" | "thurs" | "thursday" => Some(5),
        "fri" | "friday" => Some(6),
        "sat" | "saturday" => Some(7),
        _ => None,
    }
}

fn interval_text(spec: &Parsed) -> Option<String> {
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

fn clock_text(spec: &Parsed, timezone: &str, at: DateTime<Utc>) -> Option<String> {
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

fn ordinal(day: u32) -> String {
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

impl Atom {
    fn is_any(&self) -> bool {
        matches!(self, Atom::Any)
    }
}

#[cfg(test)]
#[path = "english_tests.rs"]
mod tests;
