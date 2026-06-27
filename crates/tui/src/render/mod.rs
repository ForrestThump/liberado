//! Rendering layer for the Liberado TUI.
//!
//! The public [`draw`] entry point orchestrates the frame: fill background,
//! compute layout, then dispatch to each pane's renderer.

pub mod chat;
pub mod input;
pub mod sidebar_conversations;
pub mod sidebar_reactions;
pub mod sidebar_status;
pub mod status_bar;

use ratatui::{Frame, style::Style};
use ratatui::widgets::Block;

use crate::app::App;
use crate::tuning::*;
use crate::ui::c;

/// Top-level draw: fill background, compute dynamic layout, dispatch to sub-renderers.
pub fn draw(frame: &mut Frame, app: &mut App, spinner_tick: u8) {
    // Clone here so the immutable borrow of the theme doesn't conflict with the
    // mutable borrow of `app` needed by `input::draw`.
    let th = app.theme.clone();

    fill_background(frame, &th);

    let layout = compute_layout(frame.area(), app);
    store_layout_rects(app, &layout);

    chat::draw(frame, layout.chat, app, &th, spinner_tick);
    sidebar_status::draw(frame, layout.sidebar_status, app, &th);
    sidebar_reactions::draw(frame, layout.sidebar_reactions, app, &th);
    sidebar_conversations::draw(frame, layout.sidebar_conversations, app, &th);
    status_bar::draw(frame, layout.status_bar, app, spinner_tick, &th);
    input::draw(frame, layout.input, app, &th);
}

// ── Background ───────────────────────────────────────────────────────

fn fill_background(frame: &mut Frame, th: &liberado_theme::Theme) {
    let bg = c(&th.app_bg, "#0d0d1a");
    frame.render_widget(Block::default().style(Style::default().bg(bg)), frame.area());
}

// ── Layout ───────────────────────────────────────────────────────────

struct Layout {
    chat: ratatui::layout::Rect,
    sidebar_full: ratatui::layout::Rect,
    input: ratatui::layout::Rect,
    status_bar: ratatui::layout::Rect,
    sidebar_status: ratatui::layout::Rect,
    sidebar_reactions: ratatui::layout::Rect,
    sidebar_conversations: ratatui::layout::Rect,
}

/// Compute every pane rectangle from the terminal area, adjusting the
/// input height to fit the current text content.
fn compute_layout(terminal: ratatui::layout::Rect, app: &App) -> Layout {
    let input_height = compute_input_height(terminal.width, &app.input);

    // Outer: [main area | bottom strip (status + input)]
    let outer = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Min(1),
            ratatui::layout::Constraint::Length(STATUS_BAR_HEIGHT + input_height),
        ])
        .split(terminal);

    // Main: [chat | sidebar]
    let main = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([
            ratatui::layout::Constraint::Percentage(CHAT_SIDEBAR_SPLIT_CHAT),
            ratatui::layout::Constraint::Percentage(CHAT_SIDEBAR_SPLIT_SIDEBAR),
        ])
        .split(outer[0]);

    // Bottom strip: [status bar | input]
    let bottom = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(STATUS_BAR_HEIGHT),
            ratatui::layout::Constraint::Length(input_height),
        ])
        .split(outer[1]);

    // Sidebar: [status | reactions | conversations]
    let sidebar_cols = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(SIDEBAR_STATUS_HEIGHT),
            ratatui::layout::Constraint::Min(SIDEBAR_REACTIONS_MIN_HEIGHT),
            ratatui::layout::Constraint::Min(SIDEBAR_CONVERSATIONS_MIN_HEIGHT),
        ])
        .split(main[1]);

    Layout {
        chat: main[0],
        sidebar_full: main[1],
        input: bottom[1],
        status_bar: bottom[0],
        sidebar_status: sidebar_cols[0],
        sidebar_reactions: sidebar_cols[1],
        sidebar_conversations: sidebar_cols[2],
    }
}

/// How tall the input area must be to display `input` without clipping.
fn compute_input_height(terminal_width: u16, input: &str) -> u16 {
    let content_width = terminal_width.saturating_sub(2) as usize; // minus borders
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
                    // Ceiling division: wrapped segments this logical line occupies.
                    ((chars + content_width - 1) / content_width) as u16
                }
            })
            .sum::<u16>()
            .max(1)
    };
    (content_lines + 2).clamp(INPUT_MIN_HEIGHT, INPUT_MAX_HEIGHT)
}

// ── State sync ───────────────────────────────────────────────────────

/// Copy the computed rectangles back into `app.layout` so input handlers
/// can reference the current dimensions.
fn store_layout_rects(app: &mut App, layout: &Layout) {
    app.layout.chat = layout.chat;
    app.layout.sidebar_full = layout.sidebar_full;
    app.layout.input = layout.input;
    app.layout.sidebar_conversations = layout.sidebar_conversations;
    app.layout.input_content_width = layout.input.width.saturating_sub(2) as usize;
}
