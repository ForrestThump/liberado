use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{Datelike, NaiveDate};

use super::date::parse_ymd;
use super::walk::markdown_notes;

/// How far past today an event still appears. The window is inclusive.
pub(crate) const EVENT_HORIZON_DAYS: i64 = 7;
const EVENT_LIMIT: usize = 10;

const WEEKDAY_LETTERS: [char; 7] = ['M', 'T', 'W', 'R', 'F', 'S', 'U'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VaultEvent {
    pub(crate) title: String,
    pub(crate) start: NaiveDate,
    pub(crate) end: NaiveDate,
    pub(crate) start_time: Option<String>,
    pub(crate) end_time: Option<String>,
    pub(crate) all_day: bool,
    pub(crate) path: String,
}

pub(crate) fn event_ping_message(root: &Path, today: NaiveDate, zone: &str) -> String {
    let mut events = Vec::new();
    for (rel, text) in markdown_notes(root) {
        events.extend(events_in_note(&rel, &text, today));
    }
    events.sort_by(event_rank);
    events.truncate(EVENT_LIMIT);
    render_events(&events, today, zone)
}

fn event_rank(left: &VaultEvent, right: &VaultEvent) -> Ordering {
    let start = left.start.cmp(&right.start);
    if start != Ordering::Equal {
        return start;
    }
    let time = left.start_time.cmp(&right.start_time);
    if time != Ordering::Equal {
        return time;
    }
    let title = left.title.cmp(&right.title);
    if title != Ordering::Equal {
        return title;
    }
    left.path.cmp(&right.path)
}

fn events_in_note(rel: &str, text: &str, today: NaiveDate) -> Vec<VaultEvent> {
    let Some(yaml) = liberado_common::extract_frontmatter(text) else {
        return Vec::new();
    };
    let map = yaml_map(yaml);
    if is_completed(map.get("completed").map(String::as_str)) {
        return Vec::new();
    }
    if is_recurring(&map) && map.contains_key("daysOfWeek") {
        return expand_recurring(rel, text, &map, today);
    }
    single_event(rel, text, &map)
        .filter(|event| event_in_window(event, today))
        .into_iter()
        .collect()
}

pub(crate) fn event_in_window(event: &VaultEvent, today: NaiveDate) -> bool {
    event.end >= today && event.start <= horizon_end(today)
}

fn horizon_end(today: NaiveDate) -> NaiveDate {
    today
        .checked_add_signed(chrono::Duration::days(EVENT_HORIZON_DAYS))
        .unwrap_or(today)
}

fn is_recurring(map: &BTreeMap<String, String>) -> bool {
    if let Some(value) = map.get("type")
        && value.eq_ignore_ascii_case("recurring")
    {
        return true;
    }
    map.contains_key("daysOfWeek")
}

fn expand_recurring(
    rel: &str,
    text: &str,
    map: &BTreeMap<String, String>,
    today: NaiveDate,
) -> Vec<VaultEvent> {
    let letters = map
        .get("daysOfWeek")
        .map(|value| weekday_letters(value))
        .unwrap_or_default();
    if letters.is_empty() {
        return Vec::new();
    }
    let start_recur = map.get("startRecur").and_then(|value| parse_ymd(value));
    let end_recur = map.get("endRecur").and_then(|value| parse_ymd(value));
    let title = event_title(text, map, rel);
    let start_time = map.get("startTime").and_then(|value| clock_token(value));
    let end_time = map.get("endTime").and_then(|value| clock_token(value));
    let all_day = all_day_flag(map, start_time.is_some(), end_time.is_some());
    let mut events = Vec::new();
    for date in dates_in_window(today, start_recur, end_recur, &letters) {
        events.push(VaultEvent {
            title: title.clone(),
            start: date,
            end: date,
            start_time: start_time.clone(),
            end_time: end_time.clone(),
            all_day,
            path: rel.to_string(),
        });
    }
    events
}

fn dates_in_window(
    today: NaiveDate,
    start_recur: Option<NaiveDate>,
    end_recur: Option<NaiveDate>,
    letters: &[char],
) -> Vec<NaiveDate> {
    let mut cursor = today;
    if let Some(start) = start_recur
        && start > today
    {
        cursor = start;
    }
    let horizon = horizon_end(today);
    let mut last = horizon;
    if let Some(end) = end_recur
        && end < horizon
    {
        last = end;
    }
    let mut out = Vec::new();
    while cursor <= last {
        if letters.contains(&weekday_letter(cursor)) {
            out.push(cursor);
        }
        let Some(next) = cursor.checked_add_signed(chrono::Duration::days(1)) else {
            break;
        };
        cursor = next;
    }
    out
}

fn weekday_letter(date: NaiveDate) -> char {
    WEEKDAY_LETTERS[date.weekday().num_days_from_monday() as usize]
}

fn weekday_letters(value: &str) -> Vec<char> {
    let mut letters = Vec::new();
    for letter in value.chars() {
        if !letter.is_ascii_alphabetic() {
            continue;
        }
        let letter = letter.to_ascii_uppercase();
        if WEEKDAY_LETTERS.contains(&letter) {
            letters.push(letter);
        }
    }
    letters
}

fn single_event(rel: &str, text: &str, map: &BTreeMap<String, String>) -> Option<VaultEvent> {
    if !is_event_shape(map, rel) {
        return None;
    }
    let start = parse_ymd(map.get("date")?)?;
    let mut end = map
        .get("endDate")
        .and_then(|value| parse_ymd(value))
        .unwrap_or(start);
    if end < start {
        end = start;
    }
    let start_time = map.get("startTime").and_then(|value| clock_token(value));
    let end_time = map.get("endTime").and_then(|value| clock_token(value));
    Some(VaultEvent {
        title: event_title(text, map, rel),
        start,
        end,
        all_day: all_day_flag(map, start_time.is_some(), end_time.is_some()),
        start_time,
        end_time,
        path: rel.to_string(),
    })
}

fn is_event_shape(map: &BTreeMap<String, String>, rel: &str) -> bool {
    let Some(date) = map.get("date") else {
        return false;
    };
    if parse_ymd(date).is_none() {
        return false;
    }
    for key in ["allDay", "startTime", "endTime", "endDate"] {
        if map.contains_key(key) {
            return true;
        }
    }
    in_calendar_folder(rel) && titled(map.get("title").map(String::as_str))
}

fn in_calendar_folder(rel: &str) -> bool {
    for component in PathBuf::from(rel).components() {
        if component.as_os_str().eq_ignore_ascii_case("calendar") {
            return true;
        }
    }
    false
}

fn titled(title: Option<&str>) -> bool {
    let Some(value) = title else {
        return false;
    };
    let value = value.trim();
    !value.is_empty() && !value.eq_ignore_ascii_case("null")
}

fn is_completed(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    !matches!(
        value.to_ascii_lowercase().as_str(),
        "null" | "~" | "false" | "no" | "off"
    )
}

fn all_day_flag(map: &BTreeMap<String, String>, has_start: bool, has_end: bool) -> bool {
    match map.get("allDay") {
        Some(value) => is_true(value),
        None => !has_start && !has_end,
    }
}

fn is_true(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "on"
    )
}

fn event_title(text: &str, map: &BTreeMap<String, String>, rel: &str) -> String {
    if let Some(title) = map.get("title") {
        let title = title.trim();
        if !title.is_empty() && !title.eq_ignore_ascii_case("null") {
            return title.to_string();
        }
    }
    if let Some(title) = heading_title(text) {
        return title;
    }
    if let Some(stem) = Path::new(rel).file_stem() {
        let stem = stem.to_string_lossy();
        if !stem.is_empty() {
            return stem.into_owned();
        }
    }
    rel.to_string()
}

fn heading_title(text: &str) -> Option<String> {
    for line in liberado_common::body_after_frontmatter(text).lines() {
        let Some(title) = line.trim().strip_prefix("# ") else {
            continue;
        };
        let title = title.trim();
        if !title.is_empty() {
            return Some(title.to_string());
        }
    }
    None
}

fn yaml_map(yaml: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in yaml.lines() {
        let line = line.trim().trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(' ') {
            continue;
        }
        map.insert(key.to_string(), unquote(value.trim()));
    }
    map
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return value[1..value.len() - 1].to_string();
    }
    value.to_string()
}

fn clock_token(value: &str) -> Option<String> {
    let value = value.trim();
    let (hour, rest) = value.split_once(':')?;
    if hour.is_empty() || hour.len() > 2 || rest.len() < 2 {
        return None;
    }
    let minute = &rest[..2];
    if !hour.chars().all(|c| c.is_ascii_digit()) || !minute.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hour: u8 = hour.parse().ok()?;
    let minute: u8 = minute.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(format!("{hour:02}:{minute:02}"))
}

fn render_events(events: &[VaultEvent], today: NaiveDate, zone: &str) -> String {
    if events.is_empty() {
        return format!("No events today or in the next {EVENT_HORIZON_DAYS} days ({zone}).");
    }
    let through = horizon_end(today);
    let mut lines = vec![format!("Events through {through} ({zone})"), String::new()];
    for (index, event) in events.iter().enumerate() {
        lines.push(format!(
            "{}. {} — {} — {}",
            index + 1,
            when_label(event),
            event.title,
            event.path
        ));
    }
    lines.join("\n")
}

fn when_label(event: &VaultEvent) -> String {
    let dates = if event.end > event.start {
        format!("{}–{}", event.start, event.end)
    } else {
        event.start.to_string()
    };
    let clock = clock_label(event);
    if clock.is_empty() {
        dates
    } else {
        format!("{dates} {clock}")
    }
}

fn clock_label(event: &VaultEvent) -> String {
    if event.all_day {
        return "all day".to_string();
    }
    match (&event.start_time, &event.end_time) {
        (Some(start), Some(end)) => format!("{start}–{end}"),
        (Some(start), None) => start.clone(),
        (None, Some(end)) => format!("until {end}"),
        (None, None) => String::new(),
    }
}
