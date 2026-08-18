//! Rendering layer for the Liberado TUI.
//!
//! Default layout is intentionally sparse:
//!   [ status bar ]
//!   [ chat       ]
//!   [ input      ]
//!
//! Prior sessions are not always-on chrome — `/session` opens a full-screen browser.
//! When a goal session is joined, a compact sidebar appears to the right of the chat pane
//! showing live gate votes, the active role, and the last validation result.

pub mod chat;
pub mod goal_sidebar;
pub mod input;
pub mod models;
pub mod sessions;
pub mod slash_palette;
pub mod status_bar;
pub mod switcher;

// Kept for reference / possible reuse; not drawn in the default layout.
#[allow(dead_code)]
pub mod sidebar_conversations;
#[allow(dead_code)]
pub mod sidebar_reactions;
#[allow(dead_code)]
pub mod sidebar_status;

use ratatui::widgets::Block;
use ratatui::{Frame, style::Style};

use crate::app::{App, Focus};
use crate::tuning::*;
use crate::ui::c;

/// Top-level draw: fill background, compute layout, dispatch to sub-renderers.
pub fn draw(frame: &mut Frame, app: &mut App, spinner_tick: u8) {
    let th = app.theme.clone();

    fill_background(frame, &th);

    if app.focus == Focus::SessionBrowser {
        let area = frame.area();
        app.layout.session_browser = area;
        app.layout.status_bar = ratatui::layout::Rect::default();
        app.layout.chat = ratatui::layout::Rect::default();
        app.layout.input = ratatui::layout::Rect::default();
        app.layout.goal_sidebar = ratatui::layout::Rect::default();
        sessions::draw(frame, area, app, &th);
        return;
    }

    if app.focus == Focus::SessionSwitcher {
        let area = frame.area();
        app.layout.session_browser = area;
        app.layout.status_bar = ratatui::layout::Rect::default();
        app.layout.chat = ratatui::layout::Rect::default();
        app.layout.input = ratatui::layout::Rect::default();
        app.layout.goal_sidebar = ratatui::layout::Rect::default();
        switcher::draw(frame, area, app, &th);
        return;
    }

    if app.focus == Focus::ModelBrowser {
        let area = frame.area();
        app.layout.session_browser = area; // reuse full-screen rect for mouse
        app.layout.status_bar = ratatui::layout::Rect::default();
        app.layout.chat = ratatui::layout::Rect::default();
        app.layout.input = ratatui::layout::Rect::default();
        app.layout.goal_sidebar = ratatui::layout::Rect::default();
        models::draw(frame, area, app, &th);
        return;
    }

    let layout = compute_layout(frame.area(), app);
    store_layout_rects(app, &layout);

    status_bar::draw(frame, layout.status_bar, app, spinner_tick, &th);
    chat::draw(frame, layout.chat, app, &th, spinner_tick);
    goal_sidebar::draw(frame, layout.goal_sidebar, app, &th);
    input::draw(frame, layout.input, app, &th);
    slash_palette::draw(frame, layout.input, app, &th);
}

/// Distinct color per `SessionKind` for the at-a-glance chip — theme-driven, so it tracks
/// `/theme` changes. Shared by the status bar, the switcher, and the joined view.
pub(crate) fn kind_color(
    kind: chat_client_contract::SessionKind,
    th: &liberado_theme::Theme,
) -> ratatui::style::Color {
    use chat_client_contract::SessionKind as K;
    match kind {
        K::Primary => c(&th.accent, "#00ffff"),
        K::Coding => c(&th.tool_ok, "#00ff00"),
        K::Life => c(&th.md_link, "#8080ff"),
        K::Custom => c(&th.tool_name, "#ffff00"),
    }
}

fn fill_background(frame: &mut Frame, th: &liberado_theme::Theme) {
    let bg = c(&th.app_bg, "#0d0d1a");
    frame.render_widget(
        Block::default().style(Style::default().bg(bg)),
        frame.area(),
    );
}

struct Layout {
    status_bar: ratatui::layout::Rect,
    chat: ratatui::layout::Rect,
    input: ratatui::layout::Rect,
    goal_sidebar: ratatui::layout::Rect,
}

/// Vertical stack: status (top) → chat (with optional goal-sidebar split) → input.
fn compute_layout(terminal: ratatui::layout::Rect, app: &App) -> Layout {
    let input_height = compute_input_height(terminal.width, &app.input, app.input_max_height);

    let chunks = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(STATUS_BAR_HEIGHT),
            ratatui::layout::Constraint::Min(1),
            ratatui::layout::Constraint::Length(input_height),
        ])
        .split(terminal);

    let chat_area = chunks[1];
    let (chat, goal_sidebar) = if app.joined.is_some() && chat_area.width >= 60 {
        let h_chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([
                ratatui::layout::Constraint::Percentage(CHAT_SIDEBAR_SPLIT_CHAT),
                ratatui::layout::Constraint::Percentage(CHAT_SIDEBAR_SPLIT_SIDEBAR),
            ])
            .split(chat_area);
        (h_chunks[0], h_chunks[1])
    } else {
        (chat_area, ratatui::layout::Rect::default())
    };

    Layout {
        status_bar: chunks[0],
        chat,
        input: chunks[2],
        goal_sidebar,
    }
}

fn compute_input_height(terminal_width: u16, input: &str, max_height: u16) -> u16 {
    let content_width = terminal_width.saturating_sub(2) as usize;
    let content_lines: u16 = if input.is_empty() {
        1
    } else {
        input
            .lines()
            .map(|line| {
                let chars = line.chars().count();
                if chars == 0 || content_width == 0 {
                    1u16
                } else {
                    chars.div_ceil(content_width) as u16
                }
            })
            .sum::<u16>()
            .max(1)
    };
    (content_lines + 2).clamp(INPUT_MIN_HEIGHT, max_height.max(INPUT_MIN_HEIGHT))
}

fn store_layout_rects(app: &mut App, layout: &Layout) {
    app.layout.status_bar = layout.status_bar;
    app.layout.chat = layout.chat;
    app.layout.input = layout.input;
    app.layout.goal_sidebar = layout.goal_sidebar;
    app.layout.session_browser = ratatui::layout::Rect::default();
    app.layout.input_content_width = layout.input.width.saturating_sub(2) as usize;
}

/// Shared fixtures for render tests: an `App` that never touches the user's config, a
/// `TestBackend` buffer renderer, and wire-type constructors. Every pane test renders the real
/// draw path into an in-memory buffer and asserts on the flattened text.
#[cfg(test)]
pub(crate) mod test_support {
    use chat_client_contract::{DomainWire, GoalHeaderSpec, SessionSummary};
    use liberado_theme::ThemeRegistry;
    use ratatui::{Terminal, backend::TestBackend};

    use crate::app::App;

    pub fn app() -> App {
        let mut app = App::new("http://127.0.0.1:4201".to_string(), ThemeRegistry::new());
        app.settings_path = None; // never touch the user's real config during tests
        app
    }

    /// Render one pane's draw function into a `w x h` buffer and flatten it to text.
    pub fn render_pane<F: FnOnce(&mut ratatui::Frame)>(w: u16, h: u16, f: F) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(f).unwrap();
        let buf = terminal.backend().buffer();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    /// Render a pane and return `(symbol, fg, bg)` per cell so style-only differences (selection
    /// highlight, ghost text) are observable.
    pub fn render_pane_styled<F: FnOnce(&mut ratatui::Frame)>(
        w: u16,
        h: u16,
        f: F,
    ) -> Vec<Vec<(String, ratatui::style::Color, ratatui::style::Color)>> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(f).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| {
                        let cell = &buf[(x, y)];
                        (cell.symbol().to_string(), cell.fg, cell.bg)
                    })
                    .collect()
            })
            .collect()
    }

    /// A goal session row for the unified switcher / session browser.
    pub fn goal_session(
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

    /// A goal-less chat row.
    pub fn chat_session(id: &str, title: &str) -> SessionSummary {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_client_contract::SessionKind;
    use ratatui::layout::Rect;

    fn theme() -> liberado_theme::Theme {
        test_support::app().theme
    }

    #[test]
    fn input_height_grows_with_multiline_input_up_to_the_cap() {
        let (w, max) = (80u16, 5u16);
        assert_eq!(compute_input_height(w, "", max), 3);
        assert_eq!(compute_input_height(w, "one line", max), 3);
        // 5 lines of ≤78 chars → 7 rows, clamped to max+2 borders.
        let five_lines = vec!["x".repeat(40); 5].join("\n");
        assert_eq!(compute_input_height(w, &five_lines, max), max.max(3));
        assert_eq!(
            compute_input_height(4, "0123456789", max),
            5,
            "wraps at width-2"
        );
        assert_eq!(compute_input_height(0, "x", max), 3, "degenerate width");
    }

    #[test]
    fn layout_keeps_status_chat_input_stack() {
        let app = test_support::app();
        let layout = compute_layout(Rect::new(0, 0, 80, 24), &app);
        assert_eq!(layout.status_bar.height, STATUS_BAR_HEIGHT);
        assert_eq!(layout.status_bar.y, 0);
        assert!(layout.input.y > layout.chat.y);
        assert_eq!(
            layout.chat.height + layout.input.height + STATUS_BAR_HEIGHT,
            24
        );
        // No joined session → no goal sidebar.
        assert_eq!(layout.goal_sidebar.width, 0);
    }

    #[test]
    fn joined_session_splits_the_chat_pane_when_wide_enough() {
        use chat_client_contract::DomainWire;
        let mut app = test_support::app();
        let joined = crate::app::JoinedSession {
            id: "g1".into(),
            kind: SessionKind::Coding,
            status: "running".into(),
            finished: false,
            description: String::new(),
            messages: Vec::new(),
            stream_buf: String::new(),
            awaiting: None,
            gate_votes: Vec::new(),
            active_role: None,
            last_validation: None,
        };
        app.joined = Some(joined);
        let narrow = compute_layout(Rect::new(0, 0, 40, 24), &app);
        assert_eq!(narrow.goal_sidebar.width, 0, "too narrow for a sidebar");
        let wide = compute_layout(Rect::new(0, 0, 100, 24), &app);
        assert!(wide.goal_sidebar.width > 0, "wide enough for a sidebar");
        let _ = DomainWire::Coding; // fixture parity with other render tests
    }

    #[test]
    fn kind_color_is_theme_driven_and_distinct() {
        let th = theme();
        let primary = kind_color(SessionKind::Primary, &th);
        let coding = kind_color(SessionKind::Coding, &th);
        assert_ne!(primary, coding);
        assert_eq!(
            kind_color(SessionKind::Life, &th),
            kind_color(SessionKind::Life, &th)
        );
    }

    #[test]
    fn default_layout_renders_status_chat_and_input() {
        let mut app = test_support::app();
        let out = test_support::render_pane(80, 24, |f| draw(f, &mut app, 0));
        assert!(out.contains("Liberado"), "status bar:\n{out}");
        assert!(out.contains("Message"), "input title:\n{out}");
        // The joined-less default has no goal sidebar split.
        assert!(
            !out.contains("Status"),
            "no sidebar in default layout:\n{out}"
        );
    }
}
