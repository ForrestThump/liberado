//! Normalize a task line so copied wording can be compared.
//!
//! The match rule itself lives in `task_dedupe`. This file only strips the
//! parts that are not the task: backrefs, tags, Tasks emoji, prose due dates,
//! and clocks.

#[path = "task_line_dates.rs"]
mod dates;

pub(super) fn note_backrefs(line: &str) -> Vec<String> {
    let mut refs = Vec::new();
    push_paren_refs(line, &mut refs);
    push_wiki_refs(line, &mut refs);
    refs
}

pub(super) fn normalize(text: &str) -> String {
    let without_refs = strip_backrefs(text);
    let without_tags = strip_hashes(&without_refs);
    let without_emoji = strip_known_emoji(&without_tags);
    let softened = soften(&without_emoji);
    let without_due = dates::strip_prose_due(&softened);
    let without_dates = dates::strip_iso_dates(&without_due);
    collapse(&without_dates).to_lowercase()
}

fn soften(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '*' || ch == '—' || ch == '–' {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

fn collapse(text: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out
}

fn strip_backrefs(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if let Some(end) = backref_end(&chars, index) {
            index = end;
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn backref_end(chars: &[char], index: usize) -> Option<usize> {
    if starts_with(chars, index, &['[', '[']) {
        return wiki_end(chars, index + 2);
    }
    if starts_with(chars, index, &['*', '(']) {
        return paren_end(chars, index + 2);
    }
    None
}

fn starts_with(chars: &[char], index: usize, prefix: &[char]) -> bool {
    if index + prefix.len() > chars.len() {
        return false;
    }
    for (offset, expected) in prefix.iter().enumerate() {
        if chars[index + offset] != *expected {
            return false;
        }
    }
    true
}

fn wiki_end(chars: &[char], mut index: usize) -> Option<usize> {
    while index + 1 < chars.len() {
        if chars[index] == ']' && chars[index + 1] == ']' {
            return Some(index + 2);
        }
        index += 1;
    }
    None
}

fn paren_end(chars: &[char], mut index: usize) -> Option<usize> {
    while index + 1 < chars.len() {
        if chars[index] == ')' && chars[index + 1] == '*' {
            return Some(index + 2);
        }
        index += 1;
    }
    None
}

fn push_paren_refs(line: &str, refs: &mut Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if starts_with(&chars, index, &['*', '('])
            && let Some(end) = paren_end(&chars, index + 2)
        {
            let inner: String = chars[index + 2..end - 2].iter().collect();
            if looks_like_path(&inner) {
                refs.push(inner);
            }
            index = end;
            continue;
        }
        index += 1;
    }
}

fn push_wiki_refs(line: &str, refs: &mut Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if starts_with(&chars, index, &['[', '['])
            && let Some(end) = wiki_end(&chars, index + 2)
        {
            let inner: String = chars[index + 2..end - 2].iter().collect();
            refs.push(wiki_target(&inner));
            index = end;
            continue;
        }
        index += 1;
    }
}

fn wiki_target(inner: &str) -> String {
    let no_alias = inner.split('|').next().unwrap_or(inner);
    no_alias
        .split('#')
        .next()
        .unwrap_or(no_alias)
        .trim()
        .to_string()
}

fn looks_like_path(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.contains('/') || trimmed.to_ascii_lowercase().ends_with(".md")
}

fn strip_hashes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '#' && hash_starts_tag(&chars, index) {
            index += 1;
            while index < chars.len() && is_tag_char(chars[index]) {
                index += 1;
            }
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn hash_starts_tag(chars: &[char], index: usize) -> bool {
    index == 0 || chars[index - 1].is_whitespace()
}

fn is_tag_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '/' || ch == '-'
}

fn strip_known_emoji(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some((index, emoji)) = next_metadata_emoji(rest) {
        out.push_str(&rest[..index]);
        let after = &rest[index + emoji.len_utf8()..];
        rest = skip_metadata_value(emoji, after);
    }
    out.push_str(rest);
    out
}

fn next_metadata_emoji(text: &str) -> Option<(usize, char)> {
    for (index, ch) in text.char_indices() {
        if is_metadata_emoji(ch) {
            return Some((index, ch));
        }
    }
    None
}

fn is_metadata_emoji(ch: char) -> bool {
    matches!(
        ch,
        '📅' | '⏳' | '🛫' | '✅' | '❌' | '➕' | '🔁' | '🔺' | '⏫' | '🔼' | '🔽'
    )
}

fn is_date_emoji(ch: char) -> bool {
    matches!(ch, '📅' | '⏳' | '🛫' | '✅' | '❌' | '➕')
}

fn skip_metadata_value(emoji: char, after: &str) -> &str {
    if emoji == '🔁' {
        return skip_until_emoji(after);
    }
    if is_date_emoji(emoji) {
        return dates::skip_date_token(after);
    }
    after
}

fn skip_until_emoji(text: &str) -> &str {
    for (index, ch) in text.char_indices() {
        if is_metadata_emoji(ch) {
            return &text[index..];
        }
    }
    ""
}
