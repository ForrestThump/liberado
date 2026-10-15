use std::cmp::Ordering;
use std::fs;
use std::path::Path;

use chrono::{NaiveDate, NaiveTime};
use turbovault_core::{TaskItem, TaskPriority, TaskStatus};
use turbovault_parser::parse_tasks;
use turbovault_plugin_tasks::TasksConfig;

use super::date::parse_ymd;
use super::walk::markdown_notes;

const OBSIDIAN_TASKS_DATA_PATH: &str = ".obsidian/plugins/obsidian-tasks-plugin/data.json";

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
}

pub(crate) fn task_ping_message(root: &Path, limit: u32, today: NaiveDate) -> String {
    let config = load_tasks_config(root);
    let mut tasks = Vec::new();
    for (rel, text) in markdown_notes(root) {
        collect_tasks(&rel, &text, today, &config, &mut tasks);
    }
    tasks.sort_by(task_rank);
    render_tasks(&tasks, limit, today)
}

fn collect_tasks(
    rel: &str,
    text: &str,
    today: NaiveDate,
    config: &TasksConfig,
    out: &mut Vec<OpenTask>,
) {
    let source_lines = task_source_lines(text);
    for (index, task) in parse_tasks_for_jobs(text).into_iter().enumerate() {
        if !config.accepts(&task) || !task.status.is_open() {
            continue;
        }
        let text = config.display_text(&task);
        if text.trim().is_empty() {
            continue;
        }
        let start = task.start_date;
        if matches!(start, Some(date) if date > today) {
            continue;
        }
        let line = source_lines.get(index).copied().unwrap_or_default();
        let due_at = clock_after(line, '📅');
        let scheduled_at = clock_after(line, '⏳');
        out.push(OpenTask {
            priority: task_priority(task.priority),
            due: task.due_date,
            due_at,
            scheduled: task.scheduled_date,
            scheduled_at,
            start,
            recurrence: task.recurrence.clone(),
            in_progress: task.status == TaskStatus::InProgress,
            text,
            path: rel.to_string(),
        });
    }
}

pub(super) fn load_tasks_config(root: &Path) -> TasksConfig {
    let path = root.join(OBSIDIAN_TASKS_DATA_PATH);
    let Ok(bytes) = fs::read(path) else {
        return TasksConfig::default();
    };
    TasksConfig::from_obsidian_data(&bytes).unwrap_or_default()
}

pub(super) fn parse_tasks_for_jobs(text: &str) -> Vec<TaskItem> {
    let normalized = normalize_task_times_for_parser(text);
    parse_tasks(&normalized)
}

fn normalize_task_times_for_parser(text: &str) -> String {
    text.lines()
        .map(|line| {
            if is_task_line(line.trim_start()) {
                normalize_task_line_for_parser(line)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_task_line_for_parser(line: &str) -> String {
    let mut out = Vec::new();
    let parts: Vec<&str> = line.split_whitespace().collect();
    let mut index = 0;
    while index < parts.len() {
        let token = parts[index];
        out.push(token.to_string());
        if is_date_marker(token)
            && let Some((next, rest)) = date_with_optional_clock(&parts, index + 1)
        {
            out.push(next.to_string());
            index = rest;
            continue;
        }
        index += 1;
    }
    out.join(" ")
}

fn is_date_marker(token: &str) -> bool {
    matches!(token, "📅" | "⏳" | "🛫" | "✅" | "❌" | "➕")
}

fn date_with_optional_clock<'a>(parts: &'a [&str], index: usize) -> Option<(&'a str, usize)> {
    let date_token = *parts.get(index)?;
    if let Some((date, clock)) = date_token.split_once('T')
        && parse_ymd(date).is_some()
        && parse_clock(clock).is_some()
    {
        return Some((date, index + 1));
    }
    parse_ymd(date_token)?;
    if let Some(clock) = parts.get(index + 1)
        && parse_clock(clock).is_some()
    {
        return Some((date_token, index + 2));
    }
    Some((date_token, index + 1))
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

fn task_priority(priority: TaskPriority) -> u8 {
    match priority {
        TaskPriority::Highest => 5,
        TaskPriority::High => 4,
        TaskPriority::Medium => 3,
        TaskPriority::Normal => 2,
        TaskPriority::Low => 1,
        TaskPriority::Lowest => 0,
    }
}

/// Clock after `emoji` when the value is `YYYY-MM-DD HH:MM` or
/// `YYYY-MM-DDTHH:MM`. Seconds are ignored. A later word that is not a clock
/// stays off the label.
fn clock_after(line: &str, emoji: char) -> Option<NaiveTime> {
    let rest = line.split(emoji).nth(1)?;
    let mut parts = rest.split_whitespace();
    let token = parts.next()?;
    if let Some(clock) = iso_clock(token) {
        return Some(clock);
    }
    let _date = parse_ymd(token)?;
    parts.next().and_then(parse_clock)
}

fn iso_clock(token: &str) -> Option<NaiveTime> {
    let (date, clock) = token.split_once('T')?;
    let _date = parse_ymd(date)?;
    parse_clock(clock)
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
