use std::cmp::Ordering;
use std::path::Path;

use chrono::{NaiveDate, NaiveTime};

use super::date::parse_ymd;
use super::walk::markdown_notes;

#[path = "task_dedupe.rs"]
mod dedupe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenTask {
    pub(crate) priority: u8,
    pub(crate) due: Option<NaiveDate>,
    pub(crate) due_at: Option<NaiveTime>,
    pub(crate) scheduled: Option<NaiveDate>,
    pub(crate) scheduled_at: Option<NaiveTime>,
    pub(crate) start: Option<NaiveDate>,
    pub(crate) recurrence: Option<String>,
    pub(crate) in_progress: bool,
    pub(crate) text: String,
    pub(crate) path: String,
    /// `*(path)*` and `[[path]]` notes named on this line. Used only to collapse copies.
    backrefs: Vec<String>,
}

pub(crate) fn task_ping_message(root: &Path, limit: u32, today: NaiveDate) -> String {
    let mut tasks = Vec::new();
    for (rel, text) in markdown_notes(root) {
        collect_tasks(&rel, &text, today, &mut tasks);
    }
    // Review notes paste the same open line. Collapse those copies first, then rank and limit.
    let mut tasks = dedupe::collapse_copies(tasks);
    tasks.sort_by(task_rank);
    render_tasks(&tasks, limit, today)
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
        task.backrefs = dedupe::note_backrefs(line);
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
    let due = mark_after(body, '📅');
    let scheduled = mark_after(body, '⏳');
    Some(OpenTask {
        priority: task_priority(body),
        due: due.map(|mark| mark.date),
        due_at: due.and_then(|mark| mark.clock),
        scheduled: scheduled.map(|mark| mark.date),
        scheduled_at: scheduled.and_then(|mark| mark.clock),
        start: mark_after(body, '🛫').map(|mark| mark.date),
        recurrence: recurrence_text(body),
        in_progress: state,
        text: body.to_string(),
        path: String::new(),
        backrefs: Vec::new(),
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

#[derive(Clone, Copy)]
struct Mark {
    date: NaiveDate,
    clock: Option<NaiveTime>,
}

/// Date after `emoji`, plus a clock when the value is `YYYY-MM-DD HH:MM` or
/// `YYYY-MM-DDTHH:MM`. Seconds are ignored. A later word that is not a clock
/// stays off the label.
fn mark_after(line: &str, emoji: char) -> Option<Mark> {
    let rest = line.split(emoji).nth(1)?;
    let mut parts = rest.split_whitespace();
    let token = parts.next()?;
    if let Some(mark) = iso_mark(token) {
        return Some(mark);
    }
    Some(Mark {
        date: parse_ymd(token)?,
        clock: parts.next().and_then(parse_clock),
    })
}

fn iso_mark(token: &str) -> Option<Mark> {
    let (date, clock) = token.split_once('T')?;
    Some(Mark {
        date: parse_ymd(date)?,
        clock: Some(parse_clock(clock)?),
    })
}

fn parse_clock(value: &str) -> Option<NaiveTime> {
    let (hour, rest) = value.split_once(':')?;
    if hour.is_empty() || hour.len() > 2 || rest.len() < 2 {
        return None;
    }
    let minute = &rest[..2];
    if !hour.chars().all(|c| c.is_ascii_digit()) || !minute.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    NaiveTime::from_hms_opt(hour.parse().ok()?, minute.parse().ok()?, 0)
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

fn task_rank(left: &OpenTask, right: &OpenTask) -> Ordering {
    let priority = right.priority.cmp(&left.priority);
    if priority != Ordering::Equal {
        return priority;
    }
    let due = sort_key(left).cmp(&sort_key(right));
    if due != Ordering::Equal {
        return due;
    }
    left.text.cmp(&right.text)
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
        parts.push(due_label(due, task.due_at, today));
    }
    if let Some(scheduled) = task.scheduled {
        parts.push(when_label("scheduled", scheduled, task.scheduled_at));
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

fn due_label(due: NaiveDate, clock: Option<NaiveTime>, today: NaiveDate) -> String {
    let label = when_label("due", due, clock);
    if due < today {
        format!("{label} (overdue)")
    } else if due == today {
        format!("{label} (today)")
    } else {
        label
    }
}

fn when_label(kind: &str, date: NaiveDate, clock: Option<NaiveTime>) -> String {
    match clock {
        Some(time) => format!("{kind} {date} at {}", time.format("%H:%M")),
        None => format!("{kind} {date}"),
    }
}
