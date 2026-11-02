//! Stand-alone notification title and body copy for push and Telegram.

use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;

use crate::kind;
use crate::plan::OpenRow;
use crate::source::OutboxSource;

pub const DIGEST_ITEMS_CAP: usize = 10;

/// Human label for a schedule `name` slug (e.g. `morning-tasks` → `Morning tasks`).
pub fn humanize_schedule_name(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(capitalize_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize_word(word: &str) -> String {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first
        .to_uppercase()
        .chain(chars.flat_map(|c| c.to_lowercase()))
        .collect()
}

pub fn format_time_12h(zone: Tz, at: DateTime<Utc>) -> String {
    let local = at.with_timezone(&zone);
    let hour = local.hour();
    let minute = local.minute();
    let (hour12, suffix) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{hour12}:{minute:02} {suffix}")
}

pub fn task_due_title(label: &str, task_text: &str) -> String {
    let prefix = match label {
        "scheduled" => "Scheduled now",
        "start" => "Starting now",
        _ => "Due now",
    };
    format!("{prefix}: {}", task_text.trim())
}

pub fn task_due_body(label: &str, path: &str, at: DateTime<Utc>, zone: Tz, late: bool) -> String {
    let clock = format_time_12h(zone, at);
    if late {
        return format!("Missed at {clock} · {path}");
    }
    let kind_word = match label {
        "scheduled" => "Task scheduled",
        "start" => "Task starting",
        _ => "Task due",
    };
    format!("{kind_word} {clock} · {path}")
}

pub fn event_start_title(event_title: &str) -> String {
    format!("Starting now: {}", event_title.trim())
}

pub fn event_start_body(path: &str, at: DateTime<Utc>, zone: Tz, late: bool) -> String {
    let clock = format_time_12h(zone, at);
    if late {
        format!("Missed at {clock} · {path}")
    } else {
        format!("Event at {clock} · {path}")
    }
}

pub fn habit_title(schedule_name: &str) -> String {
    humanize_schedule_name(schedule_name)
}

pub fn habit_body(habit_text: &str, schedule_name: &str) -> String {
    format!(
        "{} · {} routine",
        habit_text.trim(),
        humanize_schedule_name(schedule_name)
    )
}

pub fn task_list_title(open_count: usize, schedule_name: &str) -> String {
    if open_count > 0 {
        format!("Open tasks ({open_count})")
    } else {
        humanize_schedule_name(schedule_name)
    }
}

pub fn event_list_title(event_count: usize, schedule_name: &str) -> String {
    if event_count > 0 {
        format!("Upcoming events ({event_count})")
    } else {
        humanize_schedule_name(schedule_name)
    }
}

pub fn job_error_title(schedule_name: &str) -> String {
    format!("{} failed", humanize_schedule_name(schedule_name))
}

pub fn test_notification_body(zone_name: &str, now: DateTime<Utc>) -> String {
    let zone = parse_zone(zone_name);
    let clock = format_time_12h(zone, now);
    format!("Test notification from Notifications settings · {clock}")
}

pub fn digest_title(count: usize) -> String {
    format!("{count} missed reminders")
}

pub fn row_is_late(row: &OpenRow, now: DateTime<Utc>) -> bool {
    row.catch_up || now.signed_duration_since(row.due_at) > chrono::Duration::minutes(5)
}

/// Title and body shown for one open row at delivery time (applies late copy when needed).
pub fn present_row(row: &OpenRow, now: DateTime<Utc>, zone_name: &str) -> (String, String, bool) {
    let zone = parse_zone(zone_name);
    let late = row_is_late(row, now);
    if !late {
        return (row.title.clone(), row.body.clone(), false);
    }
    let (title, body) = late_copy(row, zone, late);
    (title, body, late)
}

fn late_copy(row: &OpenRow, zone: Tz, late: bool) -> (String, String) {
    match row.kind.as_str() {
        kind::TASK_DUE => {
            if let Some(OutboxSource::VaultNote { path, label, text }) = &row.source {
                (
                    task_due_title(label, text),
                    task_due_body(label, path, row.due_at, zone, late),
                )
            } else {
                (row.title.clone(), row.body.clone())
            }
        }
        kind::EVENT_START => {
            if let Some(OutboxSource::VaultNote { path, text, .. }) = &row.source {
                (
                    event_start_title(text),
                    event_start_body(path, row.due_at, zone, late),
                )
            } else {
                (row.title.clone(), row.body.clone())
            }
        }
        _ => (row.title.clone(), row.body.clone()),
    }
}

pub fn digest_items(rows: &[OpenRow], now: DateTime<Utc>, zone_name: &str) -> Vec<DigestItem> {
    rows.iter()
        .take(DIGEST_ITEMS_CAP)
        .map(|row| {
            let (title, _, _) = present_row(row, now, zone_name);
            DigestItem {
                outbox_id: row.id,
                kind: row.kind.clone(),
                title,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DigestItem {
    pub outbox_id: i64,
    pub kind: String,
    pub title: String,
}

fn parse_zone(name: &str) -> Tz {
    name.parse().unwrap_or(chrono_tz::UTC)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::plan::OpenRow;

    #[test]
    fn task_due_copy_uses_operator_zone_and_late_wording() {
        let zone: Tz = "America/Chicago".parse().unwrap();
        let at = Utc.with_ymd_and_hms(2026, 11, 1, 20, 0, 0).unwrap();
        assert_eq!(
            task_due_title("due", "Call dentist"),
            "Due now: Call dentist"
        );
        assert_eq!(
            task_due_body("due", "Tasks/Inbox.md", at, zone, false),
            "Task due 2:00 PM · Tasks/Inbox.md"
        );
        assert_eq!(
            task_due_body("due", "Tasks/Inbox.md", at, zone, true),
            "Missed at 2:00 PM · Tasks/Inbox.md"
        );
    }

    #[test]
    fn digest_title_uses_missed_wording() {
        assert_eq!(digest_title(4), "4 missed reminders");
    }

    #[test]
    fn present_row_leaves_on_time_rows_unchanged() {
        let row = OpenRow {
            id: 1,
            channel: "webpush".into(),
            target: "sub:1".into(),
            title: "Due now: x".into(),
            body: "Task due 1:00 PM · a.md".into(),
            catch_up: false,
            due_at: Utc.with_ymd_and_hms(2026, 11, 1, 18, 0, 0).unwrap(),
            token: "t".into(),
            kind: kind::TASK_DUE.into(),
            source: None,
        };
        let now = row.due_at;
        let (title, body, late) = present_row(&row, now, "UTC");
        assert_eq!(title, row.title);
        assert_eq!(body, row.body);
        assert!(!late);
    }
}
