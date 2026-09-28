use std::cmp::Ordering;
use std::path::Path;

use chrono::NaiveDate;

use super::date::parse_ymd;
use super::walk::markdown_notes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenTask {
    pub(crate) priority: u8,
    pub(crate) due: Option<NaiveDate>,
    pub(crate) scheduled: Option<NaiveDate>,
    pub(crate) start: Option<NaiveDate>,
    pub(crate) recurrence: Option<String>,
    pub(crate) in_progress: bool,
    pub(crate) text: String,
    pub(crate) path: String,
}

pub(crate) fn task_ping_message(root: &Path, limit: u32, today: NaiveDate) -> String {
    let mut tasks = Vec::new();
    for (rel, text) in markdown_notes(root) {
        collect_tasks(&rel, &text, today, &mut tasks);
    }
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
