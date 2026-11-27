use std::cmp::Ordering;
use std::fs;
use std::path::Path;

use jiff::civil::{Date, Time};

use super::date::{date_from_display, format_hhmm, parse_ymd, time_hms};
use super::walk::markdown_notes;
use turbovault_core::{TaskItem, TaskPriority, TaskStatus};
use turbovault_parser::parse_tasks;
use turbovault_plugin_tasks::TasksConfig;

const OBSIDIAN_TASKS_DATA_PATH: &str = ".obsidian/plugins/obsidian-tasks-plugin/data.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenTask {
    pub(crate) priority: u8,
    pub(crate) due: Option<Date>,
    pub(crate) due_at: Option<Time>,
    pub(crate) scheduled: Option<Date>,
    pub(crate) scheduled_at: Option<Time>,
    pub(crate) start: Option<Date>,
    pub(crate) recurrence: Option<String>,
    pub(crate) in_progress: bool,
    pub(crate) text: String,
    pub(crate) path: String,
}

pub(crate) fn task_ping_message(root: &Path, limit: u32, today: Date) -> String {
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
    today: Date,
    config: &TasksConfig,
    out: &mut Vec<OpenTask>,
) {
    for task in parse_tasks_for_jobs(text) {
        if let Some(open_task) = open_task_from_parsed(rel, today, config, task) {
            out.push(open_task);
        }
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
    let normalized = normalize_task_lines_for_parser(text);
    parse_tasks(&normalized)
}

/// Rewrite the task lines the parser cannot read as they are.
///
/// TurboVault's parser reads a time of day as its own token, so
/// `📅 2026-09-28 14:00` and `📅 2026-09-28 🕘 14:00` both parse. Two shapes
/// need a rewrite: an ISO date-time is one token
/// (`📅 2026-09-28T14:00`), and a time with seconds is not a time of day the
/// parser can read (`📅 2026-09-28 14:00:30`).
fn normalize_task_lines_for_parser(text: &str) -> String {
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
        index += 1;
        if !is_date_marker(token) {
            continue;
        }
        let Some(date) = parts
            .get(index)
            .copied()
            .filter(|token| parse_ymd(token).is_some())
        else {
            continue;
        };
        match split_iso_datetime(date) {
            Some((date, time)) => {
                out.push(date);
                out.push(time);
                index += 1;
            }
            None => {
                out.push(date.to_string());
                index += 1;
                if let Some(clock) = parts.get(index).and_then(|token| parse_clock(token)) {
                    out.push(format_hhmm(clock));
                    index += 1;
                }
            }
        }
    }
    out.join(" ")
}

fn is_date_marker(token: &str) -> bool {
    matches!(token, "📅" | "⏳" | "🛫" | "✅" | "❌" | "➕")
}

/// Split an ISO date-time (`2026-09-28T14:00`) into a date and a time.
///
/// The parser reads a time of day as its own token, so the ISO spelling has
/// to be split before the parser sees it. Seconds are dropped.
fn split_iso_datetime(token: &str) -> Option<(String, String)> {
    let (date, time) = token.split_once('T')?;
    parse_ymd(date)?;
    let clock = parse_clock(time)?;
    Some((date.to_string(), format_hhmm(clock)))
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
    const PRIORITY_ORDER: [TaskPriority; 6] = [
        TaskPriority::Lowest,
        TaskPriority::Low,
        TaskPriority::Normal,
        TaskPriority::Medium,
        TaskPriority::High,
        TaskPriority::Highest,
    ];
    PRIORITY_ORDER
        .iter()
        .position(|candidate| *candidate == priority)
        .unwrap_or(2) as u8
}

fn open_task_from_parsed(
    rel: &str,
    today: Date,
    config: &TasksConfig,
    task: TaskItem,
) -> Option<OpenTask> {
    if !config.accepts(&task) || !task.status.is_open() {
        return None;
    }
    let text = config.display_text(&task);
    if text.trim().is_empty() {
        return None;
    }
    let start = task.start_date.as_ref().and_then(date_from_display);
    if matches!(start, Some(date) if date > today) {
        return None;
    }
    Some(OpenTask {
        priority: task_priority(task.priority),
        due: task.due_date.as_ref().and_then(date_from_display),
        due_at: task_time(task.due_time.as_deref()),
        scheduled: task.scheduled_date.as_ref().and_then(date_from_display),
        scheduled_at: task_time(task.scheduled_time.as_deref()),
        start,
        recurrence: task.recurrence.clone(),
        in_progress: task.status == TaskStatus::InProgress,
        text,
        path: rel.to_string(),
    })
}

/// The time of day TurboVault found for a date.
///
/// TurboVault reports it as `HH:MM` and leaves it empty when the task has no
/// time.
fn task_time(value: Option<&str>) -> Option<Time> {
    value.and_then(parse_clock)
}

fn parse_clock(value: &str) -> Option<Time> {
    let (hour, rest) = value.split_once(':')?;
    if hour.is_empty() || hour.len() > 2 || rest.len() < 2 {
        return None;
    }
    let minute = &rest[..2];
    if !hour.chars().all(|c| c.is_ascii_digit()) || !minute.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    time_hms(hour, minute, 0)
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

fn sort_key(task: &OpenTask) -> (bool, Date) {
    match task.due.or(task.scheduled) {
        Some(date) => (false, date),
        None => (true, Date::MIN),
    }
}

fn render_tasks(tasks: &[OpenTask], limit: u32, today: Date) -> String {
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

fn format_task_line(index: usize, task: &OpenTask, today: Date) -> String {
    let mut parts = vec![format!("{index}. {}", task.text)];
    if let Some(due) = task.due {
        parts.push(due_label(due, task.due_at, today));
    } else if let Some(label) = when_label("due", None, task.due_at) {
        parts.push(label);
    }
    if let Some(label) = when_label("scheduled", task.scheduled, task.scheduled_at) {
        parts.push(label);
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

fn due_label(due: Date, clock: Option<Time>, today: Date) -> String {
    let label = date_label("due", due, clock);
    if due < today {
        format!("{label} (overdue)")
    } else if due == today {
        format!("{label} (today)")
    } else {
        label
    }
}

/// The label for a date and the time of day that belongs to it.
///
/// A time of day without a date still gets a label: the vault carries
/// `[dueTime:: 09:00]` on a task that has no due date.
fn when_label(kind: &str, date: Option<Date>, clock: Option<Time>) -> Option<String> {
    match (date, clock) {
        (Some(date), _) => Some(date_label(kind, date, clock)),
        (None, Some(clock)) => Some(format!("{kind} at {}", format_hhmm(clock))),
        (None, None) => None,
    }
}

fn date_label(kind: &str, date: Date, clock: Option<Time>) -> String {
    match clock {
        Some(time) => format!("{kind} {date} at {}", format_hhmm(time)),
        None => format!("{kind} {date}"),
    }
}
