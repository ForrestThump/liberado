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
