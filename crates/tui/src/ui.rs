//! Ratatui rendering for the Liberado TUI.
//!
//! Pure functions that read `App` and draw into a `Frame`. Never mutate state — all
//! mutation goes through `App::update()` and `App::handle_key()` in `app.rs`.
//!
//! Color resolution: every color comes from `app.theme` via [`resolve_colors`]. This
//! means `/theme dark` or `/theme light` instantly changes every rendered element
//! without a restart.
//!
//! The actual pane rendering is delegated to `crate::render`.

use ratatui::{Frame, style::Color};
use liberado_theme::parse_hex;

use crate::app::App;
use crate::render;

/// Resolve a themed hex key to a ratatui `Color`.
pub(crate) fn c(key: &Option<String>, fallback: &str) -> Color {
    let hex = key.clone().unwrap_or_else(|| fallback.to_string());
    if let Some((r, g, b)) = parse_hex(&hex) {
        Color::Rgb(r, g, b)
    } else {
        Color::Gray
    }
}

/// Top-level draw — delegates to `crate::render::draw()`.
pub fn draw(frame: &mut Frame, app: &mut App, spinner_tick: u8) {
    render::draw(frame, app, spinner_tick);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::truncate_path;

    #[test]
    fn c_with_valid_hex_key() {
        let key = Some("#ff0000".to_string());
        let color = c(&key, "#000000");
        assert_eq!(color, Color::Rgb(255, 0, 0));
    }

    #[test]
    fn c_with_none_uses_fallback() {
        let color = c(&None, "#00ff00");
        assert_eq!(color, Color::Rgb(0, 255, 0));
    }

    #[test]
    fn c_with_invalid_hex_returns_gray() {
        let key = Some("not-a-color".to_string());
        let color = c(&key, "#000000");
        assert_eq!(color, Color::Gray);
    }

    #[test]
    fn c_with_invalid_fallback_returns_gray() {
        let color = c(&None, "bad");
        assert_eq!(color, Color::Gray);
    }

    #[test]
    fn truncate_path_short_unchanged() {
        assert_eq!(truncate_path("/a/b", 10), "/a/b");
    }

    #[test]
    fn truncate_path_with_separator() {
        let result = truncate_path("/home/user/projects/my-project", 20);
        assert!(result.starts_with("..."));
        assert!(result.contains("my-project") || result.contains("projects"));
    }

    #[test]
    fn truncate_path_with_backslash() {
        let result = truncate_path("C:\\Users\\Name\\Documents", 20);
        assert!(result.starts_with("..."));
    }

    #[test]
    fn truncate_path_no_separator() {
        let result = truncate_path("a-very-long-filename-without-directories", 15);
        assert!(result.starts_with("..."));
    }
}
