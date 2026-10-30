//! Minute tick for remind-marked tasks and calendar notes.
//!
//! Date-only tasks and all-day events do not fire here. Morning digests list those.
//! A task fires on the first dated anchor that is present: due, then scheduled, then
//! start. That anchor needs a clock time (`YYYY-MM-DD HH:MM` or `YYYY-MM-DDTHH:MM`,
//! seconds ignored). The operator zone supplies "now".
//!
//! Fired keys live in one JSON file outside the vault. The same local minute does not
//! send twice.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use chrono::TimeZone;
use chrono_tz::Tz;
use turbovault_plugin_tasks::TasksConfig;

use liberado_outbox::{content_id, task_key};

use super::date::parse_ymd;
use super::local_time;
use super::events::events_in_note;
use super::remind::remind_opt_in;
use super::tasks::{load_tasks_config, parse_tasks_for_jobs};
use super::walk::markdown_notes;
use liberado_config::VaultRemindersConfig;

/// Separates kind, vault path, local minute, and id in one fire key.
/// U+001F is the ASCII unit separator. A path or title that contains that
/// character would share a key with a different note.
const KEY_SEP: char = '\u{1f}';

/// Clock and switches for one tick. The daemon fills this from topology.
pub(crate) struct ReminderTick {
    pub now: NaiveDateTime,
    pub settings: VaultRemindersConfig,
    pub state_path: PathBuf,
    pub zone: String,
}

#[derive(Clone)]
pub(crate) struct DueReminderRow {
    pub idempotency_key: String,
    pub title: String,
    pub body: String,
    pub due_at: DateTime<Utc>,
}

struct DueItem {
    idempotency_key: String,
    legacy_key: String,
    line: String,
    due_at: DateTime<Utc>,
}

struct Stamp {
    date: NaiveDate,
    time: Option<NaiveTime>,
    label: &'static str,
}

/// Rows for the outbox. Idempotency replaces the JSON minute file on this path.
pub(crate) fn due_reminder_rows(root: &Path, tick: &ReminderTick) -> Vec<DueReminderRow> {
    if !tick.settings.enabled {
        return Vec::new();
    }
    let zone = parse_zone(&tick.zone);
    collect(root, tick, zone)
        .into_iter()
        .map(|item| DueReminderRow {
            idempotency_key: item.idempotency_key,
            title: "Reminder".into(),
            body: item.line,
            due_at: item.due_at,
        })
        .collect()
}

/// `None` when there is nothing to send (legacy path without outbox).
pub(crate) fn reminder_message(root: &Path, tick: &ReminderTick) -> Option<String> {
    if !tick.settings.enabled {
        return None;
    }
    let zone = parse_zone(&tick.zone);
    let items = collect(root, tick, zone);
    let fired = load_fires(&tick.state_path);
    let fresh: Vec<&DueItem> = items
        .iter()
        .filter(|item| !fired.contains(&item.legacy_key))
        .collect();
    if fresh.is_empty() {
        return None;
    }
    let mut next = fired;
    for item in &fresh {
        next.insert(item.legacy_key.clone());
    }
    save_fires(&tick.state_path, &prune(next, tick.now));
    Some(render(&fresh))
}

fn parse_zone(name: &str) -> Tz {
    name.parse().unwrap_or(chrono_tz::UTC)
}

fn collect(root: &Path, tick: &ReminderTick, zone: Tz) -> Vec<DueItem> {
    let tasks_config = load_tasks_config(root);
    let mut items = Vec::new();
    for (rel, text) in markdown_notes(root) {
        if tick.settings.tasks {
            items.extend(tasks_due(
                &rel,
                &text,
                tick.now,
                zone,
                &tasks_config,
            ));
        }
        if tick.settings.events {
            items.extend(events_due(&rel, &text, tick.now, zone));
        }
    }
    items
}

fn tasks_due(
    rel: &str,
    text: &str,
    now: NaiveDateTime,
    zone: Tz,
    config: &TasksConfig,
) -> Vec<DueItem> {
    let lines = task_source_lines(text);
    parse_tasks_for_jobs(text)
        .into_iter()
        .enumerate()
        .filter_map(|(index, task)| due_item_for_task(rel, now, zone, config, &lines, index, task))
        .collect()
}

fn task_due(
    rel: &str,
    content: &str,
    line: &str,
    now: NaiveDateTime,
    zone: Tz,
) -> Option<DueItem> {
    if !remind_opt_in(line) {
        return None;
    }
    let stamp = anchor_stamp(line)?;
    let time = stamp.time?;
    if !local_time::same_local_minute(zone, stamp.date, time, now) {
        return None;
    }
    let when = format_when_iso(stamp.date, time);
    let due_at = local_to_utc(zone, stamp.date, time)?;
    let identity = content_id(line);
    let idempotency_key = task_key("task", rel, stamp.label, &when, &identity);
    let when_display = format_when(stamp.date, time);
    Some(DueItem {
        legacy_key: fire_key("task", rel, &when_display, &identity),
        idempotency_key,
        line: format!(
            "{} — {} {when_display} — {rel}",
            content.trim(),
            stamp.label
        ),
        due_at,
    })
}

fn due_item_for_task(
    rel: &str,
    now: NaiveDateTime,
    zone: Tz,
    config: &TasksConfig,
    lines: &[&str],
    index: usize,
    task: turbovault_core::TaskItem,
) -> Option<DueItem> {
    if !config.accepts(&task) || !task.status.is_open() {
        return None;
    }
    let line = lines.get(index).copied().unwrap_or_default();
    task_due(rel, &task.content, line, now, zone)
}

fn task_source_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| is_task_line(line.trim_start()))
        .collect()
}

fn is_task_line(line: &str) -> bool {
    line.starts_with("- [")
        || line.starts_with("* [")
        || line.starts_with("+ [")
        || starts_with_ordered_task(line)
}

fn starts_with_ordered_task(line: &str) -> bool {
    let mut chars = line.chars().peekable();
    let mut saw_digit = false;
    while matches!(chars.peek(), Some(ch) if ch.is_ascii_digit()) {
        saw_digit = true;
        chars.next();
    }
    if !saw_digit {
        return false;
    }
    matches!(chars.next(), Some('.') | Some(')'))
        && matches!(chars.next(), Some(' '))
        && matches!(chars.next(), Some('['))
}

fn anchor_stamp(line: &str) -> Option<Stamp> {
    for (emoji, label) in [('📅', "due"), ('⏳', "scheduled"), ('🛫', "start")] {
        if line.contains(emoji) {
            return stamp_after(line, emoji, label);
        }
    }
    None
}

fn stamp_after(line: &str, emoji: char, label: &'static str) -> Option<Stamp> {
    let rest = line.split(emoji).nth(1)?;
    let mut parts = rest.split_whitespace();
    let token = parts.next()?;
    if let Some((date, time)) = split_date_time(token) {
        return Some(Stamp {
            date,
            time: Some(time),
            label,
        });
    }
    let date = parse_ymd(token)?;
    let time = parts.next().and_then(parse_clock);
    Some(Stamp { date, time, label })
}

fn split_date_time(token: &str) -> Option<(NaiveDate, NaiveTime)> {
    let (date, time) = token.split_once('T')?;
    Some((parse_ymd(date)?, parse_clock(time)?))
}

fn parse_clock(value: &str) -> Option<NaiveTime> {
    let (hour, rest) = value.trim().split_once(':')?;
    if hour.is_empty() || hour.len() > 2 || rest.len() < 2 {
        return None;
    }
    let minute = &rest[..2];
    if !hour.chars().all(|c| c.is_ascii_digit()) || !minute.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn format_when(date: NaiveDate, time: NaiveTime) -> String {
    format!("{} {}", date, time.format("%H:%M"))
}

fn format_when_iso(date: NaiveDate, time: NaiveTime) -> String {
    format!("{}T{}", date, time.format("%H:%M"))
}

fn local_to_utc(zone: Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    let local = NaiveDateTime::new(date, time);
    match zone.from_local_datetime(&local) {
        chrono::LocalResult::Single(value) => Some(value.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
        chrono::LocalResult::None => None,
    }
}

fn events_due(rel: &str, text: &str, now: NaiveDateTime, zone: Tz) -> Vec<DueItem> {
    if !remind_opt_in(text) {
        return Vec::new();
    }
    let mut items = Vec::new();
    for event in events_in_note(rel, text, now.date()) {
        let Some(clock) = event.start_time.as_deref().and_then(parse_clock) else {
            continue;
        };
        if event.start != now.date()
            || !local_time::same_local_minute(zone, event.start, clock, now)
        {
            continue;
        }
        let when = format_when_iso(event.start, clock);
        let due_at = local_to_utc(zone, event.start, clock).unwrap_or_else(Utc::now);
        let title = event.title.trim();
        let idempotency_key = task_key("event", rel, "scheduled", &when, title);
        let when_display = format_when(event.start, clock);
        items.push(DueItem {
            legacy_key: fire_key("event", rel, &when_display, title),
            idempotency_key,
            line: format!("{} — {when_display} — {rel}", event.title),
            due_at,
        });
    }
    items
}

fn fire_key(kind: &str, path: &str, when: &str, id: &str) -> String {
    format!("{kind}{KEY_SEP}{path}{KEY_SEP}{when}{KEY_SEP}{id}")
}

fn render(items: &[&DueItem]) -> String {
    let mut lines = vec!["Reminders".to_string(), String::new()];
    for item in items {
        lines.push(format!("- {}", item.line));
    }
    lines.join("\n")
}

fn load_fires(path: &Path) -> BTreeSet<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return BTreeSet::new();
    };
    let Some(list) = value.get("fires").and_then(|v| v.as_array()) else {
        return BTreeSet::new();
    };
    list.iter()
        .filter_map(|item| item.as_str().map(str::to_string))
        .collect()
}

/// Best-effort overwrite. A crash during the write can leave partial JSON.
/// The next load then sees no fires and may send that minute again.
fn save_fires(path: &Path, fires: &BTreeSet<String>) {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(error) = fs::create_dir_all(parent)
    {
        tracing::warn!(%error, path = %parent.display(), "reminder dedupe directory");
        return;
    }
    let body = serde_json::json!({
        "fires": fires.iter().collect::<Vec<_>>(),
    });
    if let Err(error) = fs::write(path, body.to_string()) {
        tracing::warn!(%error, path = %path.display(), "reminder dedupe write");
    }
}

fn prune(fires: BTreeSet<String>, now: NaiveDateTime) -> BTreeSet<String> {
    fires.into_iter().filter(|key| keep_key(key, now)).collect()
}

fn keep_key(key: &str, now: NaiveDateTime) -> bool {
    let Some(when) = key.split(KEY_SEP).nth(2) else {
        return false;
    };
    let Some(stamp) = NaiveDateTime::parse_from_str(when, "%Y-%m-%d %H:%M").ok() else {
        return false;
    };
    let Some(cutoff) = now.checked_sub_signed(chrono::Duration::hours(36)) else {
        return true;
    };
    stamp >= cutoff
}

#[cfg(test)]
#[path = "../vault_reminder_tests.rs"]
mod vault_reminder_tests;
