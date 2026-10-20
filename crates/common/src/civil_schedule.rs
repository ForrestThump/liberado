//! Weekday-and-clock schedules compiled to a 7-field cron expression.
//!
//! The expression's fields are civil time in a named IANA zone. The cron crate
//! evaluates them in that zone. This module does not walk fire times.

use chrono::Weekday;

/// Named weekdays at one clock time. `days` is unique and Monday-first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CivilSchedule {
    pub days: Vec<Weekday>,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Parse `days` (`tue`, `Tuesday`) and `at` (`20:00` or `20:00:00`).
pub fn parse_civil(days: &[String], at: &str) -> Result<CivilSchedule, String> {
    if days.is_empty() {
        return Err("days must list at least one weekday".into());
    }
    let mut seen = [false; 7];
    let mut parsed = Vec::with_capacity(days.len());
    for raw in days {
        let day = parse_day(raw)?;
        let index = day.num_days_from_monday() as usize;
        if seen[index] {
            return Err(format!("duplicate day '{}'", raw.trim()));
        }
        seen[index] = true;
        parsed.push(day);
    }
    parsed.sort_by_key(|day| day.num_days_from_monday());
    let (hour, minute, second) = parse_at(at)?;
    Ok(CivilSchedule {
        days: parsed,
        hour,
        minute,
        second,
    })
}

/// 7-field cron, seconds first. Weekday numbers are the cron crate's: 1=Sun … 7=Sat.
pub fn compile_cron(spec: &CivilSchedule) -> String {
    let mut dows: Vec<u32> = spec
        .days
        .iter()
        .map(|day| day.number_from_sunday())
        .collect();
    dows.sort_unstable();
    let list = join_numbers(&dows);
    format!(
        "{second} {minute} {hour} * * {list} *",
        second = spec.second,
        minute = spec.minute,
        hour = spec.hour,
    )
}

/// Inverse of [`compile_cron`] for a single civil clock. Other expressions return `None`.
pub fn parse_compiled_cron(expr: &str) -> Option<CivilSchedule> {
    let tokens: Vec<&str> = expr.split_whitespace().collect();
    if tokens.len() != 6 && tokens.len() != 7 {
        return None;
    }
    if tokens.len() == 7 && !is_star(tokens[6]) {
        return None;
    }
    if !is_star(tokens[3]) || !is_star(tokens[4]) {
        return None;
    }
    let second = plain_number(tokens[0], 59)?;
    let minute = plain_number(tokens[1], 59)?;
    let hour = plain_number(tokens[2], 23)?;
    let days = parse_dow(tokens[5])?;
    Some(CivilSchedule {
        days,
        hour,
        minute,
        second,
    })
}

/// `Every Tuesday at 8:00 PM America/Chicago`.
pub fn civil_english(spec: &CivilSchedule, timezone: &str) -> String {
    let clock = format_clock(spec.hour, spec.minute, spec.second);
    let when = format!("{} at {clock}", day_phrase(&spec.days));
    let zone = timezone.trim();
    if zone.is_empty() {
        when
    } else {
        format!("{when} {zone}")
    }
}

fn parse_day(raw: &str) -> Result<Weekday, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => Ok(Weekday::Mon),
        "tue" | "tues" | "tuesday" => Ok(Weekday::Tue),
        "wed" | "wednesday" => Ok(Weekday::Wed),
        "thu" | "thur" | "thurs" | "thursday" => Ok(Weekday::Thu),
        "fri" | "friday" => Ok(Weekday::Fri),
        "sat" | "saturday" => Ok(Weekday::Sat),
        "sun" | "sunday" => Ok(Weekday::Sun),
        other => Err(format!("unknown day '{other}'")),
    }
}

fn parse_at(raw: &str) -> Result<(u32, u32, u32), String> {
    let text = raw.trim();
    let mut parts = text.split(':');
    let hour = parts.next().unwrap_or("");
    let minute = match parts.next() {
        Some(value) => value,
        None => return Err(format!("invalid time '{text}'")),
    };
    let second = parts.next().unwrap_or("0");
    if parts.next().is_some() {
        return Err(format!("invalid time '{text}'"));
    }
    let hour = clock_part(hour, text)?;
    let minute = clock_part(minute, text)?;
    let second = clock_part(second, text)?;
    if hour > 23 || minute > 59 || second > 59 {
        return Err(format!("invalid time '{text}'"));
    }
    Ok((hour, minute, second))
}

fn clock_part(raw: &str, text: &str) -> Result<u32, String> {
    if raw.is_empty() || !raw.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(format!("invalid time '{text}'"));
    }
    raw.parse::<u32>()
        .map_err(|_| format!("invalid time '{text}'"))
}

fn is_star(token: &str) -> bool {
    token == "*" || token == "?"
}

fn plain_number(token: &str, max: u32) -> Option<u32> {
    if token.is_empty() || !token.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let value = token.parse::<u32>().ok()?;
    (value <= max).then_some(value)
}

fn parse_dow(token: &str) -> Option<Vec<Weekday>> {
    if is_star(token) {
        return Some(all_days());
    }
    let mut days = Vec::new();
    for part in token.split(',') {
        let number = plain_number(part, 7)?;
        let day = weekday_from_cron(number)?;
        if days.contains(&day) {
            return None;
        }
        days.push(day);
    }
    if days.is_empty() {
        return None;
    }
    days.sort_by_key(|day| day.num_days_from_monday());
    Some(days)
}

fn weekday_from_cron(number: u32) -> Option<Weekday> {
    match number {
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

fn all_days() -> Vec<Weekday> {
    vec![
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ]
}

fn join_numbers(values: &[u32]) -> String {
    let mut text = String::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&value.to_string());
    }
    text
}

fn day_phrase(days: &[Weekday]) -> String {
    if days.len() == 7 {
        return "Every day".to_string();
    }
    if days
        == [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ]
    {
        return "Weekdays".to_string();
    }
    let names: Vec<&str> = days.iter().copied().map(weekday_name).collect();
    match names.as_slice() {
        [one] => format!("Every {one}"),
        [left, right] => format!("Every {left} and {right}"),
        _ => join_oxford(&names),
    }
}

fn join_oxford(names: &[&str]) -> String {
    let Some((last, rest)) = names.split_last() else {
        return "Every day".to_string();
    };
    format!("Every {}, and {last}", rest.join(", "))
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

fn format_clock(hour: u32, minute: u32, second: u32) -> String {
    let (clock, suffix) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    if second == 0 {
        format!("{clock}:{minute:02} {suffix}")
    } else {
        format!("{clock}:{minute:02}:{second:02} {suffix}")
    }
}

#[cfg(test)]
#[path = "civil_schedule_tests.rs"]
mod tests;
