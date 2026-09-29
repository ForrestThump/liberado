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

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};

use super::date::parse_ymd;
use super::events::events_in_note;
use super::remind::remind_opt_in;
use super::tasks::parse_task_line;
use super::walk::markdown_notes;
use liberado_config::VaultRemindersConfig;

const KEY_SEP: char = '\u{1f}';

/// Clock and switches for one tick. The daemon fills this from topology.
pub(crate) struct ReminderTick {
    pub now: NaiveDateTime,
    pub settings: VaultRemindersConfig,
    pub state_path: PathBuf,
}

struct DueItem {
    key: String,
    line: String,
}

struct Stamp {
    date: NaiveDate,
    time: Option<NaiveTime>,
    label: &'static str,
}

/// `None` when there is nothing to send.
pub(crate) fn reminder_message(root: &Path, tick: &ReminderTick) -> Option<String> {
    if !tick.settings.enabled {
        return None;
    }
    let items = collect(root, tick);
    let fired = load_fires(&tick.state_path);
    let fresh: Vec<&DueItem> = items
        .iter()
        .filter(|item| !fired.contains(&item.key))
        .collect();
    if fresh.is_empty() {
        return None;
    }
    let mut next = fired;
    for item in &fresh {
        next.insert(item.key.clone());
    }
    save_fires(&tick.state_path, &prune(next, tick.now));
    Some(render(&fresh))
}

fn collect(root: &Path, tick: &ReminderTick) -> Vec<DueItem> {
    let mut items = Vec::new();
    for (rel, text) in markdown_notes(root) {
        if tick.settings.tasks {
            items.extend(tasks_due(&rel, &text, tick.now));
        }
        if tick.settings.events {
            items.extend(events_due(&rel, &text, tick.now));
        }
    }
    items
}

fn tasks_due(rel: &str, text: &str, now: NaiveDateTime) -> Vec<DueItem> {
    let mut items = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if let Some(item) = task_due(rel, line, index, now) {
            items.push(item);
        }
    }
    items
}

fn task_due(rel: &str, line: &str, index: usize, now: NaiveDateTime) -> Option<DueItem> {
    parse_task_line(line)?;
    if !remind_opt_in(line) {
        return None;
    }
    let stamp = anchor_stamp(line)?;
    let time = stamp.time?;
    if !same_minute(stamp.date, time, now) {
        return None;
    }
    let when = format_when(stamp.date, time);
    Some(DueItem {
        key: fire_key("task", rel, &when, &index.to_string()),
        line: format!("{} — {} {when} — {rel}", task_label(line), stamp.label),
    })
}

fn task_label(line: &str) -> String {
    line.trim()
        .trim_start_matches("- [")
        .trim_start_matches([' ', '/'])
        .trim_start_matches(']')
        .trim()
        .to_string()
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

fn same_minute(date: NaiveDate, time: NaiveTime, now: NaiveDateTime) -> bool {
    now.date() == date && now.time().hour() == time.hour() && now.time().minute() == time.minute()
}

fn format_when(date: NaiveDate, time: NaiveTime) -> String {
    format!("{} {}", date, time.format("%H:%M"))
}

fn events_due(rel: &str, text: &str, now: NaiveDateTime) -> Vec<DueItem> {
    if !remind_opt_in(text) {
        return Vec::new();
    }
    let mut items = Vec::new();
    for event in events_in_note(rel, text, now.date()) {
        let Some(clock) = event.start_time.as_deref().and_then(parse_clock) else {
            continue;
        };
        if event.start != now.date() || !same_minute(event.start, clock, now) {
            continue;
        }
        let when = format_when(event.start, clock);
        items.push(DueItem {
            key: fire_key("event", rel, &when, &event.title),
            line: format!("{} — {when} — {rel}", event.title),
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
