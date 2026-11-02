//! Title, body, kind, and source for cron-originated reminder rows.

use chrono::{DateTime, Utc};

use liberado_outbox::{
    EVENT_LIST, EVENT_START, HABIT, JOB_ERROR, OutboxSource, TASK_DUE, TASK_LIST, copy,
};

use crate::jobs::JobRequest;

use super::MechanicalReminder;

pub fn from_vault(row: crate::vault_pings::DueReminderRow) -> MechanicalReminder {
    MechanicalReminder {
        idempotency_key: row.idempotency_key,
        title: row.title,
        body: row.body,
        due_at: row.due_at,
        catch_up: false,
        kind: row.kind,
        source: Some(row.source),
    }
}

pub fn from_text(
    job: &JobRequest,
    text: &str,
    catch_up: bool,
    fire_at: DateTime<Utc>,
    zone: &str,
) -> MechanicalReminder {
    let source = OutboxSource::Schedule {
        name: job.name.clone(),
    };
    let count = numbered_list_items(text);
    let (kind, title, body) = match job.kind.as_str() {
        "task-ping" => (
            TASK_LIST,
            copy::task_list_title(count, &job.name),
            text.to_string(),
        ),
        "event-ping" => (
            EVENT_LIST,
            copy::event_list_title(count, &job.name),
            text.to_string(),
        ),
        "habit-ping" => (
            HABIT,
            copy::habit_title(&job.name),
            copy::habit_body(text, &job.name),
        ),
        "git-snapshot" => (
            JOB_ERROR,
            copy::job_error_title(&job.name),
            job_error_body(text, &job.name),
        ),
        _ => (
            JOB_ERROR,
            copy::job_error_title(&job.name),
            text.to_string(),
        ),
    };
    let (title, body) = apply_catch_up_copy(kind, &source, title, body, fire_at, zone, catch_up);
    MechanicalReminder {
        idempotency_key: format!("job/{}/{}", job.kind, fire_at.to_rfc3339()),
        title,
        body,
        due_at: fire_at,
        catch_up,
        kind: kind.to_string(),
        source: Some(source),
    }
}

fn job_error_body(text: &str, schedule_name: &str) -> String {
    let prefix = format!("{}: ", schedule_name);
    text.strip_prefix(&prefix)
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .unwrap_or(text)
        .to_string()
}

fn numbered_list_items(body: &str) -> usize {
    body.lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.chars().next().is_some_and(|ch| ch.is_ascii_digit()) && trimmed.contains(". ")
        })
        .count()
}

fn apply_catch_up_copy(
    kind: &str,
    source: &OutboxSource,
    title: String,
    body: String,
    fire_at: DateTime<Utc>,
    zone_name: &str,
    catch_up: bool,
) -> (String, String) {
    if !catch_up {
        return (title, body);
    }
    let zone: chrono_tz::Tz = zone_name.parse().unwrap_or(chrono_tz::UTC);
    match (kind, source) {
        (TASK_DUE, OutboxSource::VaultNote { path, label, text }) => (
            copy::task_due_title(label, text),
            copy::task_due_body(label, path, fire_at, zone, true),
        ),
        (EVENT_START, OutboxSource::VaultNote { path, text, .. }) => (
            copy::event_start_title(text),
            copy::event_start_body(path, fire_at, zone, true),
        ),
        _ => (title, body),
    }
}
