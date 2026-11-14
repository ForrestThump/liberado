//! Match vault task lines to stored `OutboxSource::VaultNote` text.
//!
//! A one-off Done checks the box and inserts the Obsidian Tasks done date.
//! The plugin's `TaskLayoutComponent` order writes `DoneDate` immediately
//! before `BlockLink`, after every other field. A line that already contains
//! ✅ is not stamped again. A 🔁 line is not edited.

use liberado_outbox::{line_is_recurring, strip_task_metadata, task_identity_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckboxState {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LineMatch {
    Open(usize),
    Closed(usize),
}

enum LineChoice {
    Open(usize),
    Closed(usize),
}

/// What Done should do to the note.
pub enum TaskLineOutcome {
    /// Write this content. A non-recurring line was checked, or it was already checked.
    Write(String),
    /// The matched line contains 🔁. Leave the file bytes alone.
    Recurring { identity: String },
}

pub fn plan_task_edit(
    content: &str,
    task_text: &str,
    done_date: &str,
) -> Result<TaskLineOutcome, String> {
    let needle = task_text.trim();
    if needle.is_empty() {
        return Err("task text is empty".into());
    }
    match choose_line(content, needle)? {
        LineChoice::Open(index) => plan_open(content, index, done_date),
        LineChoice::Closed(index) => plan_closed(content, index),
    }
}

fn choose_line(content: &str, stored: &str) -> Result<LineChoice, String> {
    let matches = matching_lines(content, stored)?;
    let open = open_indexes(&matches);
    if open.len() > 1 {
        return Err("task line is ambiguous".into());
    }
    if let Some(index) = open.first() {
        return Ok(LineChoice::Open(*index));
    }
    let closed = closed_indexes(&matches);
    if closed.len() > 1 {
        return Err("task line is ambiguous".into());
    }
    if let Some(index) = closed.first() {
        return Ok(LineChoice::Closed(*index));
    }
    Err("no matching open task line".into())
}

fn open_indexes(matches: &[LineMatch]) -> Vec<usize> {
    matches
        .iter()
        .filter_map(|hit| match hit {
            LineMatch::Open(index) => Some(*index),
            LineMatch::Closed(_) => None,
        })
        .collect()
}

fn closed_indexes(matches: &[LineMatch]) -> Vec<usize> {
    matches
        .iter()
        .filter_map(|hit| match hit {
            LineMatch::Closed(index) => Some(*index),
            LineMatch::Open(_) => None,
        })
        .collect()
}

fn plan_open(content: &str, index: usize, done_date: &str) -> Result<TaskLineOutcome, String> {
    let line = line_at(content, index)?;
    if line_is_recurring(line) {
        return Ok(recurring(line));
    }
    Ok(TaskLineOutcome::Write(toggle_line_at(
        content, index, done_date,
    )?))
}

fn plan_closed(content: &str, index: usize) -> Result<TaskLineOutcome, String> {
    let line = line_at(content, index)?;
    if line_is_recurring(line) {
        return Ok(recurring(line));
    }
    Ok(TaskLineOutcome::Write(content.to_string()))
}

fn recurring(line: &str) -> TaskLineOutcome {
    TaskLineOutcome::Recurring {
        identity: task_identity_key(line)
            .unwrap_or_else(|| liberado_outbox::collapse_whitespace(line.trim())),
    }
}

fn line_at(content: &str, index: usize) -> Result<&str, String> {
    content
        .lines()
        .nth(index)
        .ok_or_else(|| "line out of range".to_string())
}

fn matching_lines(content: &str, stored: &str) -> Result<Vec<LineMatch>, String> {
    let mut hits = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let Some((state, description)) = parse_task_line(line) else {
            continue;
        };
        if description == stored {
            hits.push(match state {
                CheckboxState::Open => LineMatch::Open(index),
                CheckboxState::Closed => LineMatch::Closed(index),
            });
        }
    }
    Ok(hits)
}

fn parse_task_line(line: &str) -> Option<(CheckboxState, String)> {
    let trimmed = line.trim_start();
    if !is_task_line(trimmed) {
        return None;
    }
    let (state, body_start) = checkbox_tail(trimmed)?;
    let body = trimmed.get(body_start..)?.trim_start();
    let description = strip_task_metadata(body).trim().to_string();
    Some((state, description))
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

fn checkbox_tail(line: &str) -> Option<(CheckboxState, usize)> {
    for (marker, state) in [
        ("- [ ]", CheckboxState::Open),
        ("* [ ]", CheckboxState::Open),
        ("+ [ ]", CheckboxState::Open),
        ("- [x]", CheckboxState::Closed),
        ("- [X]", CheckboxState::Closed),
        ("* [x]", CheckboxState::Closed),
        ("* [X]", CheckboxState::Closed),
        ("+ [x]", CheckboxState::Closed),
        ("+ [X]", CheckboxState::Closed),
    ] {
        if let Some(idx) = line.find(marker) {
            return Some((state, idx + marker.len()));
        }
    }
    if !starts_with_ordered_task(line) {
        return None;
    }
    if let Some(idx) = line.find("[ ]") {
        return Some((CheckboxState::Open, idx + 3));
    }
    if let Some(idx) = line.find("[x]") {
        return Some((CheckboxState::Closed, idx + 3));
    }
    line.find("[X]").map(|idx| (CheckboxState::Closed, idx + 3))
}

fn toggle_line_at(content: &str, index: usize, done_date: &str) -> Result<String, String> {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let line = lines
        .get(index)
        .ok_or_else(|| "line out of range".to_string())?;
    let checked = replace_open_checkbox(line)?;
    lines[index] = append_done_date(&checked, done_date);
    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Insert ` ✅ YYYY-MM-DD` where Obsidian Tasks writes the done date.
///
/// The plugin layout puts that field immediately before a trailing `^block-id`.
/// The rest of the line stays as written, including a due time the plugin
/// serializer would drop. A second ✅ is not added.
fn append_done_date(line: &str, done_date: &str) -> String {
    if line.contains('✅') {
        return line.to_string();
    }
    let (body, trailing) = split_trailing_whitespace(line);
    let (head, block) = peel_trailing_block(body);
    let mut out = String::with_capacity(line.len() + done_date.len() + 4);
    push_done_stamp(&mut out, head, done_date, block);
    out.push_str(trailing);
    out
}

fn push_done_stamp(out: &mut String, head: &str, done_date: &str, block: Option<&str>) {
    out.push_str(head.trim_end());
    out.push_str(" ✅ ");
    out.push_str(done_date);
    if let Some(block) = block {
        out.push(' ');
        out.push_str(block);
    }
}

fn split_trailing_whitespace(line: &str) -> (&str, &str) {
    let end = line.trim_end().len();
    (&line[..end], &line[end..])
}

fn peel_trailing_block(line: &str) -> (&str, Option<&str>) {
    let Some((head, token)) = split_last_token(line) else {
        return (line, None);
    };
    if is_block_token(token) {
        (head, Some(token))
    } else {
        (line, None)
    }
}

fn split_last_token(line: &str) -> Option<(&str, &str)> {
    let idx = line.rfind(char::is_whitespace)?;
    let token_start = idx + line[idx..].chars().next()?.len_utf8();
    if token_start >= line.len() {
        return None;
    }
    Some((&line[..token_start], &line[token_start..]))
}

fn is_block_token(token: &str) -> bool {
    let Some(rest) = token.strip_prefix('^') else {
        return false;
    };
    !rest.is_empty()
        && rest
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn replace_open_checkbox(line: &str) -> Result<String, String> {
    for (open, closed) in [("- [ ]", "- [x]"), ("* [ ]", "* [x]"), ("+ [ ]", "+ [x]")] {
        if line.contains(open) {
            return Ok(line.replacen(open, closed, 1));
        }
    }
    if starts_with_ordered_task(line.trim_start())
        && let Some(idx) = line.find("[ ]")
    {
        let mut out = line.to_string();
        out.replace_range(idx..idx + 3, "[x]");
        return Ok(out);
    }
    Err("no open checkbox on matched line".into())
}

#[cfg(test)]
#[path = "vault_task_line_tests.rs"]
mod tests;
