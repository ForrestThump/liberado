//! Task-completion detector for the vault reminder tick.
//!
//! The tick already walks every note. When `[vault_reminders] completions` is
//! on, this pass counts every task the Obsidian Tasks global filter accepts
//! and writes one log row per real checkoff. The first sighting of a note only
//! seeds the snapshot. Unchanged notes are skipped by content hash.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use turbovault_core::{TaskItem, TaskStatus};
use turbovault_plugin_tasks::TasksConfig;

use liberado_outbox::{
    CompletionDraft, Outbox, SnapshotChange, TaskCounts, VoidDraft, content_fingerprint,
    diff_snapshot, task_identity_key,
};

use super::tasks::{load_tasks_config, parse_tasks_for_jobs};
use super::tick::ReminderTick;
use super::walk::markdown_notes;

#[derive(Default)]
struct Observation {
    open: i64,
    closed: i64,
    hint_text: Option<String>,
    done_date: Option<NaiveDate>,
}

/// Record checkoffs for this tick. A disabled switch, or a missing log flag, writes nothing.
pub(crate) fn record_task_completions(
    root: &Path,
    tick: &ReminderTick,
    outbox: &Outbox,
) -> Result<(), liberado_outbox::Error> {
    if !logging_enabled(&tick.settings) {
        return Ok(());
    }
    let config = load_tasks_config(root);
    let detected = detected_utc(&tick.zone, tick.now);
    for (path, text) in markdown_notes(root) {
        record_note(outbox, &path, &text, &config, detected, &tick.zone)?;
    }
    Ok(())
}

fn logging_enabled(settings: &liberado_config::VaultRemindersConfig) -> bool {
    settings.enabled && settings.completions
}

fn record_note(
    outbox: &Outbox,
    path: &str,
    text: &str,
    config: &TasksConfig,
    detected_at: DateTime<Utc>,
    zone: &str,
) -> Result<(), liberado_outbox::Error> {
    let fingerprint = content_fingerprint(text);
    if outbox.same_fingerprint(path, &fingerprint)? {
        return Ok(());
    }
    let observed = observe_note(text, config);
    let now = counts_of(&observed);
    let prev = outbox.snapshot_counts(path)?;
    let changes = diff_snapshot(&prev, &now);
    let (completions, voids) = drafts(&observed, &changes);
    outbox.apply_completion_note(&liberado_outbox::NoteApply {
        path,
        fingerprint: &fingerprint,
        counts: &now,
        completions: &completions,
        voids: &voids,
        detected_at,
        zone,
    })
}

fn observe_note(text: &str, config: &TasksConfig) -> BTreeMap<String, Observation> {
    let mut counts = BTreeMap::new();
    for task in parse_tasks_for_jobs(text) {
        if config.accepts(&task) {
            note_task(&mut counts, text, &task);
        }
    }
    counts
}

fn note_task(counts: &mut BTreeMap<String, Observation>, text: &str, task: &TaskItem) {
    if task.status == TaskStatus::Cancelled {
        return;
    }
    let Some(key) = identity_for(text, task) else {
        return;
    };
    let entry = counts.entry(key).or_default();
    if task.status.is_open() {
        entry.open += 1;
        return;
    }
    if task.status.is_completed() {
        remember_closed(entry, task);
    }
}

fn remember_closed(entry: &mut Observation, task: &TaskItem) {
    entry.closed += 1;
    if entry.hint_text.is_none() {
        let text = task.content.trim();
        if !text.is_empty() {
            entry.hint_text = Some(text.to_string());
        }
    }
    entry.done_date = latest_date(entry.done_date, task.done_date);
}

fn latest_date(current: Option<NaiveDate>, next: Option<NaiveDate>) -> Option<NaiveDate> {
    match (current, next) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (None, next) => next,
        (current, None) => current,
    }
}

fn identity_for(text: &str, task: &TaskItem) -> Option<String> {
    let line = source_line(text, task.position.line);
    if let Some(key) = task_identity_key(line) {
        return Some(key);
    }
    let collapsed = liberado_outbox::collapse_whitespace(task.content.trim());
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

fn source_line(text: &str, line_number: usize) -> &str {
    if line_number == 0 {
        return "";
    }
    text.lines().nth(line_number - 1).unwrap_or("")
}

fn counts_of(observed: &BTreeMap<String, Observation>) -> BTreeMap<String, TaskCounts> {
    observed
        .iter()
        .map(|(key, row)| {
            (
                key.clone(),
                TaskCounts {
                    open: row.open,
                    closed: row.closed,
                },
            )
        })
        .collect()
}

fn drafts(
    observed: &BTreeMap<String, Observation>,
    changes: &[SnapshotChange],
) -> (Vec<CompletionDraft>, Vec<VoidDraft>) {
    let mut completions = Vec::new();
    let mut voids = Vec::new();
    for change in changes {
        match change {
            SnapshotChange::Completed {
                task_text,
                count,
                prior_closed,
            } => completions.push(completion_draft(observed, task_text, *count, *prior_closed)),
            SnapshotChange::Voided { task_text, count } => voids.push(VoidDraft {
                task_text: task_text.clone(),
                count: *count,
            }),
        }
    }
    (completions, voids)
}

fn completion_draft(
    observed: &BTreeMap<String, Observation>,
    task_text: &str,
    count: u32,
    prior_closed: i64,
) -> CompletionDraft {
    let row = observed.get(task_text);
    CompletionDraft {
        task_text: task_text.to_string(),
        count,
        prior_closed,
        hint_text: row.and_then(|item| item.hint_text.clone()),
        done_date: row.and_then(|item| item.done_date),
    }
}

fn detected_utc(zone: &str, local: NaiveDateTime) -> DateTime<Utc> {
    let tz = parse_zone(zone);
    match tz.from_local_datetime(&local) {
        LocalResult::Single(local) => local.with_timezone(&Utc),
        LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
        LocalResult::None => shifted_utc(&tz, local),
    }
}

fn shifted_utc(zone: &chrono_tz::Tz, local: NaiveDateTime) -> DateTime<Utc> {
    let shifted = local
        .checked_add_signed(chrono::Duration::hours(1))
        .unwrap_or(local);
    match zone.from_local_datetime(&shifted) {
        LocalResult::Single(local) | LocalResult::Ambiguous(local, _) => local.with_timezone(&Utc),
        LocalResult::None => local.and_utc(),
    }
}

fn parse_zone(zone: &str) -> chrono_tz::Tz {
    zone.parse().unwrap_or(chrono_tz::UTC)
}

#[cfg(test)]
#[path = "completions_tests.rs"]
mod completions_tests;
