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

    // ── truncate_path ───────────────────────────────────────────────────

    #[test]
    fn truncate_path_keeps_short_paths_whole() {
        assert_eq!(truncate_path("/a/b.md", 20), "/a/b.md");
    }

    #[test]
    fn truncate_path_cuts_a_long_name_in_half() {
        // name.len() (12) > max/2 (5) → truncated at 5 chars.
        assert_eq!(truncate_path("/docs/verylongname.md", 10), "...veryl");
    }

    #[test]
    fn truncate_path_without_a_separator_trims_the_tail() {
        assert_eq!(truncate_path("verylongname.md", 8), "...me.md");
        assert_eq!(truncate_path("short.md", 30), "short.md");
    }

    #[test]
    fn truncate_for_display_appends_an_ellipsis() {
        assert_eq!(truncate_for_display("short", 30), "short");
        assert_eq!(truncate_for_display("a long string here", 10), "a long ...");
    }

    #[test]
    fn short_id_respects_the_length_cap() {
        let id = "0123456789abcdef";
        assert_eq!(short_id(id), "01234567");
        assert_eq!(short_id("short"), "short");
    }

    // ── relative_time (wall-clock boundaries) ────────────────────────────

    /// Sub-minute deltas render in seconds; the renderers exercise the older buckets elsewhere.
    #[test]
    fn relative_time_recent_deltas_show_seconds() {
        use chrono::{Duration, Utc};
        let now = Utc::now();
        let iso = (now - Duration::seconds(5)).to_rfc3339();
        assert_eq!(relative_time(&iso), "5s ago");
        // Future timestamps and unparseable strings pass through untouched.
        let future = (now + Duration::seconds(5)).to_rfc3339();
        assert_eq!(relative_time(&future), future);
        assert_eq!(relative_time("not-a-date"), "not-a-date");
    }
}
