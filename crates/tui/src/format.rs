//! Formatting utilities for the Liberado TUI.
//!
//! Pure functions with no dependencies on App state. Shared by `app.rs`, `ui.rs`,
//! and `commands.rs`.

use crate::tuning::*;

pub fn relative_time(iso: &str) -> String {
    let Ok(ts) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return iso.to_string();
    };
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(ts);
    if delta.num_seconds() < 0 {
        return iso.to_string();
    }
    let secs = delta.num_seconds() as u64;
    if secs < RELATIVE_SECS_THRESHOLD {
        return format!("{}s ago", secs);
    }
    let mins = secs / 60;
    if mins < RELATIVE_MINS_THRESHOLD {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < RELATIVE_HOURS_THRESHOLD {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days == RELATIVE_YESTERDAY_THRESHOLD {
        return "yesterday".to_string();
    }
    if days < RELATIVE_DAYS_THRESHOLD {
        return format!("{days}d ago");
    }
    ts.format("%b %e").to_string()
}

pub fn format_uptime(seconds: u64) -> String {
    let h = seconds / SECS_IN_HOUR;
    let m = (seconds % SECS_IN_HOUR) / SECS_IN_MINUTE;
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {}s", seconds % 60)
    }
}

/// Safely truncates a string to `max` bytes at a valid UTF-8 char boundary.
pub(crate) fn safe_truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn truncate_for_display(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    format!(
        "{}...",
        safe_truncate(text, max.saturating_sub(ELLIPSIS_LEN))
    )
}

pub fn short_id(id: &str) -> &str {
    safe_truncate(id, id.len().min(SHORT_ID_LEN))
}

pub(crate) fn state_label(running: bool) -> &'static str {
    if running { "running" } else { "stopped" }
}
pub(crate) fn attached_label(attached: bool) -> &'static str {
    if attached { "attached" } else { "detached" }
}

pub fn truncate_path(path: &str, max: usize) -> String {
    if path.len() <= max {
        return path.to_string();
    }
    if let Some(idx) = path.rfind(['/', '\\']) {
        let name = &path[idx + 1..];
        let name_trunc = if name.len() > max / 2 {
            safe_truncate(name, max / 2)
        } else {
            name
        };
        format!("...{name_trunc}")
    } else {
        let start = path.len().saturating_sub(max.saturating_sub(ELLIPSIS_LEN));
        let truncated = safe_truncate(&path[start..], max.saturating_sub(ELLIPSIS_LEN));
        format!("...{truncated}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_truncate_ascii() {
        assert_eq!(safe_truncate("hello", 3), "hel");
    }
    #[test]
    fn safe_truncate_multibyte() {
        assert_eq!(safe_truncate("café", 5), "café");
    }
    #[test]
    fn safe_truncate_multibyte_cut() {
        assert_eq!(safe_truncate("café", 3), "caf");
    }
    #[test]
    fn short_id_multibyte() {
        assert_eq!(short_id("café"), "café");
    }
}
