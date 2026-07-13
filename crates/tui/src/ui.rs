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

use liberado_theme::parse_hex;
use ratatui::{Frame, style::Color};

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

    // ── Render smoke tests (session-focus S3) ────────────────────────────────
    //
    // Render the real `ui::draw` path into an in-memory `TestBackend` buffer and assert it does
    // not panic and contains the expected text. Covers the view layer (switcher, kind chip,
    // awaiting banner) that the state-machine unit tests can't — closing most of the gap a live
    // human smoke would, short of "does it look nice".

    use crate::app::{Action, App, GoalUiEvent};
    use chat_client_contract::{DomainWire, GoalHeaderSpec, SessionSummary};
    use liberado_theme::ThemeRegistry;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_to_string(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app, 0)).unwrap();
        let buf = terminal.backend().buffer();
        let area = buf.area;
        let mut s = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    fn goal_header(
        id: &str,
        domain: DomainWire,
        desc: &str,
        status: &str,
        awaiting: bool,
    ) -> SessionSummary {
        SessionSummary {
            id: id.into(),
            goal: Some(GoalHeaderSpec {
                description: desc.into(),
                domain,
            }),
            status: status.into(),
            created_at: String::new(),
            awaiting_input: awaiting,
            result: None,
            title: None,
            visibility: Default::default(),
            parent_session: None,
        }
    }

    /// A goal-less session — i.e. a chat. `goal: None` is the entire difference (D7).
    fn chat_summary(id: &str, title: &str) -> SessionSummary {
        SessionSummary {
            id: id.into(),
            goal: None,
            status: "running".into(),
            created_at: String::new(),
            awaiting_input: false,
            result: None,
            title: Some(title.into()),
            visibility: Default::default(),
            parent_session: None,
        }
    }

    fn smoke_app() -> App {
        let mut app = App::new("http://127.0.0.1:4201".to_string(), ThemeRegistry::new());
        app.settings_path = None; // never touch the user's real config during tests
        app
    }

    #[test]
    fn switcher_renders_prior_chats_kind_chips_and_status() {
        let mut app = smoke_app();
        // One list: a chat and two goal sessions, all rows of the same kind of thing (S5′).
        app.update(Action::SessionsUpdate(vec![
            chat_summary("c1", "weekly planning"),
            goal_header("g1", DomainWire::Coding, "build a CLI", "running", false),
            goal_header("g2", DomainWire::Life, "capture a note", "running", true),
        ]));
        app.open_session_switcher();
        let out = render_to_string(&mut app, 100, 20);
        // The goal-less session renders as a primary (CHAT) row; the goal ones carry their chips.
        assert!(out.contains("Primary"), "missing primary chat row:\n{out}");
        assert!(
            out.contains("weekly planning"),
            "missing chat title:\n{out}"
        );
        assert!(
            out.contains("CODE") && out.contains("Coding"),
            "missing coding chip:\n{out}"
        );
        assert!(
            out.contains("LIFE") && out.contains("Life"),
            "missing life chip:\n{out}"
        );
        // The awaiting session's status stands out.
        assert!(out.contains("awaiting"), "missing awaiting status:\n{out}");
        assert!(out.contains("build a CLI"), "missing description:\n{out}");
    }

    #[test]
    fn switcher_with_only_prior_chats_renders_them() {
        let mut app = smoke_app();
        app.update(Action::SessionsUpdate(vec![chat_summary(
            "c1",
            "weekly planning",
        )]));
        app.open_session_switcher();
        let out = render_to_string(&mut app, 80, 12);
        // Even with no goal sessions, goal-less ones (chats) populate the switcher.
        assert!(out.contains("Primary"), "missing primary chat row:\n{out}");
        assert!(
            out.contains("weekly planning"),
            "missing chat title:\n{out}"
        );
    }

    #[test]
    fn switcher_with_nothing_shows_empty_hint() {
        let mut app = smoke_app();
        app.open_session_switcher();
        let out = render_to_string(&mut app, 80, 12);
        assert!(
            out.contains("no sessions yet"),
            "missing empty hint:\n{out}"
        );
    }

    #[test]
    fn joined_view_renders_kind_header_and_awaiting_banner() {
        let mut app = smoke_app();
        app.update(Action::SessionsUpdate(vec![goal_header(
            "g1",
            DomainWire::Coding,
            "build a CLI",
            "running",
            false,
        )]));
        app.join_session("g1".to_string());
        app.update(Action::GoalStreamEvent(GoalUiEvent::Awaiting {
            prompt: "What should I title the note?".into(),
            options: vec!["Weekly Review".into()],
        }));
        let out = render_to_string(&mut app, 100, 20);
        // Header shows the kind + the awaiting marker; the banner shows the prompt + option.
        assert!(out.contains("Coding"), "missing kind in header:\n{out}");
        assert!(out.contains("awaiting"), "missing awaiting marker:\n{out}");
        assert!(
            out.contains("What should I title the note?"),
            "missing prompt banner:\n{out}"
        );
        assert!(out.contains("Weekly Review"), "missing option:\n{out}");
    }

    #[test]
    fn a_multi_line_intake_contract_renders_line_by_line() {
        // S7's freeze UI: the draft contract is a block, not a one-liner. A ratatui `Line` does not
        // break on `\n`, so without splitting, the criteria and verifiers would run together into
        // one unreadable smear — and the human would be accepting gates they never actually saw.
        let mut app = smoke_app();
        app.join_session("g1".to_string());
        app.update(Action::GoalStreamEvent(GoalUiEvent::Awaiting {
            prompt:
                "Draft contract — review before I build anything.\n\nGoal: Build a todo CLI\n\n\
                     Success criteria:\n  - add and list work\n\nVerifiers (the machine gates this \
                     will be judged against):\n  - paths: these paths must exist — src/main.rs"
                    .into(),
            options: vec!["accept".into(), "reject".into()],
        }));
        let out = render_to_string(&mut app, 100, 24);

        // Each block must land on its own row, not be concatenated into one.
        for needle in [
            "Draft contract",
            "Goal: Build a todo CLI",
            "add and list work",
            "src/main.rs",
        ] {
            assert!(
                out.lines().any(|l| l.contains(needle)),
                "'{needle}' should occupy its own rendered line:\n{out}"
            );
        }
        assert!(
            out.contains("accept") && out.contains("reject"),
            "missing verdict options:\n{out}"
        );
    }

    #[test]
    fn status_bar_shows_primary_kind_chip_by_default() {
        let mut app = smoke_app();
        let out = render_to_string(&mut app, 100, 20);
        // The at-a-glance chip for the primary chat.
        assert!(
            out.contains("CHAT") && out.contains("Primary"),
            "missing primary chip:\n{out}"
        );
    }
}
