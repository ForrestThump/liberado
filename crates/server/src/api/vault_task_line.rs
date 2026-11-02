//! Match vault task lines to stored `OutboxSource::VaultNote` text.

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

pub fn toggle_open_task_line(content: &str, task_text: &str) -> Result<String, String> {
    let needle = task_text.trim();
    if needle.is_empty() {
        return Err("task text is empty".into());
    }
    let matches = matching_lines(content, needle)?;
    let open: Vec<_> = matches
        .iter()
        .filter_map(|hit| match hit {
            LineMatch::Open(index) => Some(*index),
            LineMatch::Closed(_) => None,
        })
        .collect();
    if open.len() > 1 {
        return Err("task line is ambiguous".into());
    }
    if open.len() == 1 {
        return toggle_line_at(content, open[0]);
    }
    let closed: Vec<_> = matches
        .iter()
        .filter_map(|hit| match hit {
            LineMatch::Closed(index) => Some(*index),
            LineMatch::Open(_) => None,
        })
        .collect();
    if closed.len() == 1 {
        return Ok(content.to_string());
    }
    if closed.len() > 1 {
        return Err("task line is ambiguous".into());
    }
    Err("no matching open task line".into())
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
    ordered_checkbox_tail(line, CheckboxState::Open)
        .or_else(|| ordered_checkbox_tail(line, CheckboxState::Closed))
}

fn ordered_checkbox_tail(line: &str, want: CheckboxState) -> Option<(CheckboxState, usize)> {
    if !starts_with_ordered_task(line) {
        return None;
    }
    let bracket = line.find('[')?;
    let tail = &line[bracket..];
    let (marker, state) = match want {
        CheckboxState::Open if tail.starts_with("[ ]") => ("[ ]", CheckboxState::Open),
        CheckboxState::Closed if tail.starts_with("[x]") || tail.starts_with("[X]") => {
            let marker = &tail[..3];
            (marker, CheckboxState::Closed)
        }
        _ => return None,
    };
    Some((state, bracket + marker.len()))
}

fn strip_task_metadata(body: &str) -> &str {
    let markers = [
        " 📅", " ⏳", " 🛫", " ✅", " ❌", " 🔁", " ⏫", " 🔽", " 🔺", " #", " ^",
    ];
    let mut end = body.len();
    for marker in markers {
        if let Some(idx) = body.find(marker) {
            end = end.min(idx);
        }
    }
    body[..end].trim_end()
}

fn toggle_line_at(content: &str, index: usize) -> Result<String, String> {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let line = lines
        .get(index)
        .ok_or_else(|| "line out of range".to_string())?;
    let updated = replace_open_checkbox(line)?;
    lines[index] = updated;
    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
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
mod tests {
    use super::*;

    #[test]
    fn empty_checkbox_does_not_match_nonempty_task() {
        let note = "- [ ]\n";
        assert!(toggle_open_task_line(note, "Call dentist").is_err());
    }

    #[test]
    fn prefix_only_line_does_not_match_longer_task() {
        let note = "- [ ] Call\n";
        assert!(toggle_open_task_line(note, "Call dentist").is_err());
    }

    #[test]
    fn indented_task_matches_stored_text() {
        let note = "  - [ ] Call dentist\n";
        let out = toggle_open_task_line(note, "Call dentist").unwrap();
        assert!(out.contains("- [x] Call dentist"));
    }

    #[test]
    fn already_checked_is_idempotent() {
        let note = "- [x] Call dentist\n";
        let out = toggle_open_task_line(note, "Call dentist").unwrap();
        assert_eq!(out, note);
    }

    #[test]
    fn ambiguous_open_lines_error() {
        let note = "- [ ] Call dentist\n- [ ] Call dentist\n";
        assert!(toggle_open_task_line(note, "Call dentist").is_err());
    }

    #[test]
    fn text_changed_line_does_not_match() {
        let note = "- [ ] Call doctor\n";
        assert!(toggle_open_task_line(note, "Call dentist").is_err());
    }

    #[test]
    fn metadata_after_description_still_matches() {
        let note = "- [ ] Call dentist 📅 2026-11-01\n";
        let out = toggle_open_task_line(note, "Call dentist").unwrap();
        assert!(out.contains("- [x] Call dentist"));
    }
}
