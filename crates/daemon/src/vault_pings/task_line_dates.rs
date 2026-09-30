//! Date and clock tokens inside a task line.
//!
//! Used when comparing copied tasks. A due emoji, an ISO date, and a prose
//! `due Oct 22` are the same kind of metadata and stay out of the description.

pub(super) fn skip_date_token(text: &str) -> &str {
    let trimmed = text.trim_start();
    let ws = text.len() - trimmed.len();
    let Some(token_len) = date_like_token_len(trimmed) else {
        return text;
    };
    let mut end = ws + token_len;
    let after = &text[end..];
    let clock_src = after.trim_start();
    let clock_ws = after.len() - clock_src.len();
    if let Some(clock_len) = clock_token_len(clock_src) {
        end += clock_ws + clock_len;
    }
    &text[end..]
}

fn date_like_token_len(text: &str) -> Option<usize> {
    let token = text.split_whitespace().next()?;
    if iso_date_len(token).is_some() || slash_date_len(token).is_some() {
        return Some(token.len());
    }
    None
}

pub(super) fn strip_iso_dates(text: &str) -> String {
    let mut out = String::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            push_kept_token(&mut out, &current);
            current.clear();
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        current.push(ch);
    }
    push_kept_token(&mut out, &current);
    out
}

fn push_kept_token(out: &mut String, token: &str) {
    if token.is_empty() || iso_date_len(token).is_some() || clock_token_len(token).is_some() {
        return;
    }
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
    out.push_str(token);
}

fn iso_date_len(token: &str) -> Option<usize> {
    let date = token.split('T').next().unwrap_or(token);
    let mut parts = date.split('-');
    let year = parts.next()?;
    let month = parts.next()?;
    let day = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if year.len() == 4
        && month.len() == 2
        && day.len() == 2
        && all_digits(year)
        && all_digits(month)
        && all_digits(day)
    {
        return Some(token.len());
    }
    None
}

fn slash_date_len(token: &str) -> Option<usize> {
    let mut parts = token.split('/');
    let month = parts.next()?;
    let day = parts.next()?;
    let year = parts.next();
    if parts.next().is_some() {
        return None;
    }
    if !(1..=2).contains(&month.len()) || !(1..=2).contains(&day.len()) {
        return None;
    }
    if !all_digits(month) || !all_digits(day) {
        return None;
    }
    if let Some(year) = year
        && ((year.len() != 4 && year.len() != 2) || !all_digits(year))
    {
        return None;
    }
    Some(token.len())
}

fn clock_token_len(token: &str) -> Option<usize> {
    let (hour, rest) = token.split_once(':')?;
    if hour.is_empty() || hour.len() > 2 || !all_digits(hour) {
        return None;
    }
    let minute: String = rest.chars().take(2).collect();
    if minute.len() != 2 || !all_digits(&minute) {
        return None;
    }
    let mut len = hour.len() + 1 + 2;
    let after = &rest[2..];
    if let Some(seconds) = after.strip_prefix(':') {
        let sec: String = seconds.chars().take(2).collect();
        if sec.len() == 2 && all_digits(&sec) && seconds.len() == 2 {
            len += 3;
        }
    }
    if len == token.len() { Some(len) } else { None }
}

fn all_digits(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    for ch in value.chars() {
        if !ch.is_ascii_digit() {
            return false;
        }
    }
    true
}

pub(super) fn strip_prose_due(text: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    for (index, _) in text.char_indices() {
        if index < cursor {
            continue;
        }
        if let Some(end) = due_phrase_end(text, index) {
            out.push_str(&text[cursor..index]);
            cursor = end;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

fn due_phrase_end(text: &str, index: usize) -> Option<usize> {
    let rest = &text[index..];
    let due = rest.get(..3)?;
    if !due.eq_ignore_ascii_case("due") {
        return None;
    }
    if !word_boundary_before(text, index) || !word_boundary_after(rest, 3) {
        return None;
    }
    prose_date_end(text, index + 3)
}

fn word_boundary_before(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    match text[..index].chars().next_back() {
        Some(ch) => !ch.is_alphanumeric(),
        None => true,
    }
}

fn word_boundary_after(text: &str, at: usize) -> bool {
    match text[at..].chars().next() {
        None => true,
        Some(ch) => !ch.is_alphanumeric(),
    }
}

fn prose_date_end(text: &str, at: usize) -> Option<usize> {
    let rest = text[at..].trim_start();
    if rest.is_empty() {
        return None;
    }
    let ws = text[at..].len() - rest.len();
    let date_len = match month_date_len(rest) {
        Some(len) => len,
        None => numeric_date_len(rest)?,
    };
    Some(at + ws + date_len)
}

fn numeric_date_len(text: &str) -> Option<usize> {
    let token = text.split_whitespace().next()?;
    match iso_date_len(token) {
        Some(len) => Some(len),
        None => slash_date_len(token),
    }
}

fn month_date_len(text: &str) -> Option<usize> {
    let month_len = month_name_len(text)?;
    let rest = text[month_len..].trim_start();
    let ws = text[month_len..].len() - rest.len();
    if ws == 0 {
        return None;
    }
    Some(month_len + ws + day_len(rest)?)
}

fn month_name_len(text: &str) -> Option<usize> {
    const MONTHS: &[&str] = &[
        "september",
        "february",
        "november",
        "december",
        "january",
        "october",
        "august",
        "march",
        "april",
        "june",
        "july",
        "sept",
        "jan",
        "feb",
        "mar",
        "apr",
        "may",
        "jun",
        "jul",
        "aug",
        "sep",
        "oct",
        "nov",
        "dec",
    ];
    let lower = text.to_lowercase();
    for name in MONTHS {
        if lower.starts_with(name) && word_boundary_after(&lower, name.len()) {
            return Some(name.len());
        }
    }
    None
}

fn day_len(text: &str) -> Option<usize> {
    let mut len = 0;
    for ch in text.chars() {
        if !ch.is_ascii_digit() {
            break;
        }
        len += 1;
        if len == 2 {
            break;
        }
    }
    if len == 0 {
        return None;
    }
    let mut end = len;
    let after = &text[end..];
    for suffix in ["st", "nd", "rd", "th"] {
        if after.len() >= suffix.len() && after[..suffix.len()].eq_ignore_ascii_case(suffix) {
            end += suffix.len();
            break;
        }
    }
    if let Some(extra) = year_tail_len(&text[end..]) {
        end += extra;
    }
    Some(end)
}

fn year_tail_len(text: &str) -> Option<usize> {
    let trimmed = text.trim_start_matches([',', ' ']);
    if trimmed.len() == text.len() {
        return None;
    }
    let skipped = text.len() - trimmed.len();
    let year: String = trimmed.chars().take(4).collect();
    if year.len() == 4 && all_digits(&year) {
        return Some(skipped + 4);
    }
    None
}
