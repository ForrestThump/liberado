//! Vault notes that mechanical reminders read. No model, no second task store.
//!
//! Tasks are Obsidian Tasks lines in any note the walker is allowed to open.
//! Events are [Full Calendar](https://github.com/obsidian-community/obsidian-full-calendar)
//! frontmatter, plus a titled note under a `calendar/` folder (the vault layout in
//! `docs/spec/liberado-architecture.md`). Dates are calendar dates in the operator zone.
//! `cron_expr` stays UTC.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Datelike, NaiveDate};

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".obsidian",
    ".trash",
    "00 - Meta",
    "Legal",
    "proposals",
];

/// How far past today an event still appears. The window is inclusive.
pub(crate) const EVENT_HORIZON_DAYS: i64 = 7;
const EVENT_LIMIT: usize = 10;

const WEEKDAY_LETTERS: [char; 7] = ['M', 'T', 'W', 'R', 'F', 'S', 'U'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenTask {
    priority: u8,
    due: Option<NaiveDate>,
    scheduled: Option<NaiveDate>,
    start: Option<NaiveDate>,
    recurrence: Option<String>,
    in_progress: bool,
    text: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VaultEvent {
    title: String,
    start: NaiveDate,
    end: NaiveDate,
    start_time: Option<String>,
    end_time: Option<String>,
    all_day: bool,
    path: String,
}

pub(crate) fn task_ping_message(root: &Path, limit: u32, today: NaiveDate) -> String {
    let mut tasks = Vec::new();
    walk_notes(root, &mut |path, text| {
        let rel = rel_path(root, path);
        collect_tasks(&rel, text, today, &mut tasks);
    });
    sort_tasks(&mut tasks);
    render_tasks(&tasks, limit, today)
}

pub(crate) fn event_ping_message(root: &Path, today: NaiveDate, zone: &str) -> String {
    let mut events = Vec::new();
    walk_notes(root, &mut |path, text| {
        let rel = rel_path(root, path);
        events.extend(events_in_note(&rel, text, today));
    });
    events.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| a.start_time.cmp(&b.start_time))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.path.cmp(&b.path))
    });
    events.truncate(EVENT_LIMIT);
    render_events(&events, today, zone)
}

fn walk_notes(root: &Path, visit: &mut impl FnMut(&Path, &str)) {
    walk_dir(root, visit);
}

fn walk_dir(dir: &Path, visit: &mut impl FnMut(&Path, &str)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if skip_dir(&name) {
                continue;
            }
            walk_dir(&path, visit);
            continue;
        }
        if !name.ends_with(".md") {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            visit(&path, &text);
        }
    }
}

fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn collect_tasks(rel: &str, text: &str, today: NaiveDate, out: &mut Vec<OpenTask>) {
    for line in text.lines() {
        let Some(mut task) = parse_task_line(line) else {
            continue;
        };
        if !task_is_listed(&task, today) {
            continue;
        }
        task.path = rel.to_string();
        out.push(task);
    }
}

pub(crate) fn parse_task_line(line: &str) -> Option<OpenTask> {
    let rest = line.trim().strip_prefix("- [")?;
    let (mark, body) = rest.split_once(']')?;
    let state = match mark {
        " " => false,
        "/" => true,
        _ => return None,
    };
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    Some(OpenTask {
        priority: task_priority(body),
        due: date_after(body, '📅'),
        scheduled: date_after(body, '⏳'),
        start: date_after(body, '🛫'),
        recurrence: recurrence_text(body),
        in_progress: state,
        text: body.to_string(),
        path: String::new(),
    })
}

fn task_is_listed(task: &OpenTask, today: NaiveDate) -> bool {
    !matches!(task.start, Some(start) if start > today)
}

fn task_priority(line: &str) -> u8 {
    if line.contains('🔺') {
        4
    } else if line.contains('⏫') {
        3
    } else if line.contains('🔼') {
        2
    } else if line.contains('🔽') {
        0
    } else {
        1
    }
}

fn date_after(line: &str, emoji: char) -> Option<NaiveDate> {
    let rest = line.split(emoji).nth(1)?;
    let token = rest.split_whitespace().next()?;
    parse_ymd(token)
}

fn recurrence_text(line: &str) -> Option<String> {
    let rest = line.split('🔁').nth(1)?.trim();
    if rest.is_empty() {
        return None;
    }
    let end = rest.find(['📅', '⏳', '🛫', '✅', '❌', '➕', '⏫', '🔼', '🔽', '🔺']);
    let text = match end {
        Some(index) => rest[..index].trim(),
        None => rest,
    };
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn sort_tasks(tasks: &mut [OpenTask]) {
    tasks.sort_by(|a, b| {
        b.priority.cmp(&a.priority).then_with(|| {
            sort_key(a)
                .cmp(&sort_key(b))
                .then_with(|| a.text.cmp(&b.text))
        })
    });
}

fn sort_key(task: &OpenTask) -> (bool, NaiveDate) {
    match task.due.or(task.scheduled) {
        Some(date) => (false, date),
        None => (true, NaiveDate::MIN),
    }
}

fn render_tasks(tasks: &[OpenTask], limit: u32, today: NaiveDate) -> String {
    if tasks.is_empty() {
        return "No open tasks.".to_string();
    }
    let mut lines = vec!["Open tasks".to_string(), String::new()];
    for (index, task) in tasks.iter().take(limit as usize).enumerate() {
        lines.push(format_task_line(index + 1, task, today));
    }
    lines.push(String::new());
    lines.push("Reply in Liberado when you want one done.".to_string());
    lines.join("\n")
}

fn format_task_line(index: usize, task: &OpenTask, today: NaiveDate) -> String {
    let mut parts = vec![format!("{index}. {}", task.text)];
    if let Some(due) = task.due {
        parts.push(due_label(due, today));
    }
    if let Some(scheduled) = task.scheduled {
        parts.push(format!("scheduled {scheduled}"));
    }
    if let Some(rule) = &task.recurrence {
        parts.push(format!("repeats {rule}"));
    }
    if task.in_progress {
        parts.push("in progress".to_string());
    }
    parts.push(task.path.clone());
    parts.join(" — ")
}

fn due_label(due: NaiveDate, today: NaiveDate) -> String {
    if due < today {
        format!("due {due} (overdue)")
    } else if due == today {
        format!("due {due} (today)")
    } else {
        format!("due {due}")
    }
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
    map.get("type")
        .is_some_and(|value| value.eq_ignore_ascii_case("recurring"))
        || map.contains_key("daysOfWeek")
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
    dates_in_window(today, start_recur, end_recur, &letters)
        .into_iter()
        .map(|date| VaultEvent {
            title: title.clone(),
            start: date,
            end: date,
            start_time: start_time.clone(),
            end_time: end_time.clone(),
            all_day,
            path: rel.to_string(),
        })
        .collect()
}

fn dates_in_window(
    today: NaiveDate,
    start_recur: Option<NaiveDate>,
    end_recur: Option<NaiveDate>,
    letters: &[char],
) -> Vec<NaiveDate> {
    let mut cursor = start_recur.filter(|start| *start > today).unwrap_or(today);
    let last = end_recur
        .filter(|end| *end < horizon_end(today))
        .unwrap_or_else(|| horizon_end(today));
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
    value
        .chars()
        .filter(|letter| letter.is_ascii_alphabetic())
        .map(|letter| letter.to_ascii_uppercase())
        .filter(|letter| WEEKDAY_LETTERS.contains(letter))
        .collect()
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
    if ["allDay", "startTime", "endTime", "endDate"]
        .iter()
        .any(|key| map.contains_key(*key))
    {
        return true;
    }
    in_calendar_folder(rel) && titled(map.get("title").map(String::as_str))
}

fn in_calendar_folder(rel: &str) -> bool {
    PathBuf::from(rel)
        .components()
        .any(|component| component.as_os_str().eq_ignore_ascii_case("calendar"))
}

fn titled(title: Option<&str>) -> bool {
    title.is_some_and(|value| {
        let value = value.trim();
        !value.is_empty() && !value.eq_ignore_ascii_case("null")
    })
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
    Path::new(rel)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| rel.to_string())
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

fn parse_ymd(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    let date = value.get(..10)?;
    if value.len() > 10 {
        let boundary = value.as_bytes().get(10).copied()?;
        if boundary != b'T' && boundary != b' ' {
            return None;
        }
    }
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
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

#[cfg(test)]
#[path = "vault_pings_tests.rs"]
mod vault_pings_tests;
