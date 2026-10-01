//! Opt-in marker for a timed reminder. Liberado owns this. TurboVault does not.
//!
//! The rightmost marker in the scanned text decides:
//!
//! - `#remind` as its own tag (any case) opts in. `#reminder` and `#remind-me` do not.
//!   A nested tag `#remind/...` still opts in.
//! - `remind` or `reminder`, then `::` (Dataview) or `:` (YAML), opts in only when the
//!   value is `true`, `yes`, `y`, `1`, or `on` (any case, optional quotes). Any other
//!   value, including `false`, `no`, `n`, `0`, and `off`, opts out.
//!
//! No marker means no reminder. On a task, scan the task line. On a calendar note, scan
//! the whole note so frontmatter and a later body tag can override each other.

pub(super) fn remind_opt_in(text: &str) -> bool {
    let mut best: Option<(usize, bool)> = None;
    consider_tags(text, &mut best);
    consider_fields(text, &mut best);
    best.is_some_and(|(_, truthy)| truthy)
}

fn prefer(best: &mut Option<(usize, bool)>, start: usize, truthy: bool) {
    if best.is_none_or(|(at, _)| start >= at) {
        *best = Some((start, truthy));
    }
}

fn consider_tags(text: &str, best: &mut Option<(usize, bool)>) {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if tag_at(bytes, index) {
            prefer(best, index, true);
            index += "#remind".len();
            continue;
        }
        index += 1;
    }
}

fn tag_at(bytes: &[u8], index: usize) -> bool {
    let needle = b"#remind";
    let end = index + needle.len();
    if end > bytes.len() || !bytes[index..end].eq_ignore_ascii_case(needle) {
        return false;
    }
    if index > 0 && tag_char(bytes[index - 1]) {
        return false;
    }
    if end == bytes.len() {
        return true;
    }
    let next = bytes[end];
    next == b'/' || !tag_char(next)
}

fn tag_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn consider_fields(text: &str, best: &mut Option<(usize, bool)>) {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let at_token = index == 0 || bytes[index - 1].is_ascii_whitespace();
        if at_token && let Some((end, truthy)) = field_at(text, index) {
            prefer(best, index, truthy);
            index = end.max(index + 1);
            continue;
        }
        index += 1;
    }
}

fn field_at(text: &str, index: usize) -> Option<(usize, bool)> {
    let rest = text.get(index..)?;
    let key_len = if starts_with_key(rest, "reminder") {
        "reminder".len()
    } else if starts_with_key(rest, "remind") {
        "remind".len()
    } else {
        return None;
    };
    let after = text.get(index + key_len..)?;
    let sep = if after.starts_with("::") {
        2
    } else if after.starts_with(':') {
        1
    } else {
        return None;
    };
    let value_at = index + key_len + sep;
    let region = text.get(value_at..)?;
    let trimmed = region.trim_start();
    let skipped = region.len() - trimmed.len();
    let token_len = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let token = &trimmed[..token_len];
    let end = value_at + skipped + token_len;
    Some((end, !token.is_empty() && truthy_token(token)))
}

fn starts_with_key(text: &str, key: &str) -> bool {
    let Some(head) = text.get(..key.len()) else {
        return false;
    };
    head.eq_ignore_ascii_case(key)
}

fn truthy_token(token: &str) -> bool {
    matches!(
        unquote(token).trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "y" | "1" | "on"
    )
}

fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let open = bytes[0];
        let close = bytes[bytes.len() - 1];
        if (open == b'"' && close == b'"') || (open == b'\'' && close == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}
