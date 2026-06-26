//! Rendering layer for the Liberado TUI.
//!
//! Each module renders one pane or area. The `draw()` function in `mod.rs` splits the
//! terminal area into panes and calls the sub-renderers.

pub mod chat;
pub mod input;
pub mod sidebar_conversations;
pub mod sidebar_reactions;
pub mod sidebar_status;
pub mod status_bar;

use ratatui::Frame;

use crate::app::App;
use crate::tuning::*;

/// Top-level draw. Computes the layout and dispatches to sub-renderers.
pub fn draw(frame: &mut Frame, app: &mut App, spinner_tick: u8) {
    let th = &app.theme;

    let outer = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Min(1),
            ratatui::layout::Constraint::Length(INPUT_AREA_HEIGHT),
        ])
        .split(frame.area());

    let main = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([
            ratatui::layout::Constraint::Percentage(CHAT_SIDEBAR_SPLIT_CHAT),
            ratatui::layout::Constraint::Percentage(CHAT_SIDEBAR_SPLIT_SIDEBAR),
        ])
        .split(outer[0]);

    let chat_area = main[0];
    let sidebar_area = main[1];

    let bottom = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(STATUS_BAR_HEIGHT),
            ratatui::layout::Constraint::Length(INPUT_ROW_HEIGHT),
        ])
        .split(outer[1]);

    let status_area = bottom[0];
    let input_area = bottom[1];

    let sidebar_chunks = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(SIDEBAR_STATUS_HEIGHT),
            ratatui::layout::Constraint::Min(SIDEBAR_REACTIONS_MIN_HEIGHT),
            ratatui::layout::Constraint::Min(SIDEBAR_CONVERSATIONS_MIN_HEIGHT),
        ])
        .split(sidebar_area);
    let sidebar_conversations_rect = sidebar_chunks[2];

    chat::draw(frame, chat_area, app, th, spinner_tick);
    sidebar_status::draw(frame, sidebar_chunks[0], app, th);
    sidebar_reactions::draw(frame, sidebar_chunks[1], app, th);
    sidebar_conversations::draw(frame, sidebar_conversations_rect, app, th);
    status_bar::draw(frame, status_area, app, spinner_tick, th);
    input::draw(frame, input_area, app, th);

    app.layout.chat = chat_area;
    app.layout.sidebar_full = sidebar_area;
    app.layout.input = input_area;
    app.layout.sidebar_conversations = sidebar_conversations_rect;
}
