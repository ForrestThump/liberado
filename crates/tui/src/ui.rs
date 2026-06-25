//! Ratatui rendering for the Liberado TUI.
//!
//! Pure functions that read `App` and draw into a `Frame`. Never mutate state — all
//! mutation goes through `App::update()` in `app.rs`. The layout is fixed:
//!
//! ```text
//! ┌─ Chat pane (70%) ────────────────────┬─ Sidebar (30%) ──────┐
//! │                                      │  Status               │
//! │  messages + live stream              │  Reactions feed       │
//! │                                      │  Conversations        │
//! ├──────────────────────────────────────┤                       │
//! │  Input line                          │                       │
//! └──────────────────────────────────────┴───────────────────────┘
//! ```

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
};

use crate::app::App;

/// Top-level draw. Called by the ratatui event loop on each frame.
pub fn draw(frame: &mut Frame, app: &App) {
    todo!("draw: split into main vertical layout (chat + input) and sidebar, then delegate to sub-renderers")
}

/// Render the chat message area — scrollback history plus the in-flight streaming
/// buffer.
fn draw_chat_pane(frame: &mut Frame, area: Rect, app: &App) {
    todo!("draw_chat_pane: render messages from app.messages + app.assistant_buf, applying app.scroll_offset")
}

/// Render the composer input line at the bottom of the chat area.
fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    todo!("draw_input: render prompt, input text with cursor, streaming indicator")
}

/// Render the right sidebar: status block, reactions feed, conversation list.
fn draw_sidebar(frame: &mut Frame, area: Rect, app: &App) {
    todo!("draw_sidebar: split sidebar into status / reactions / conversations blocks")
}

/// Render the daemon status block.
fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    todo!("draw_status: running indicator, uptime, dispatcher/orchestrator attached")
}

/// Render the reactions tail.
fn draw_reactions(frame: &mut Frame, area: Rect, app: &App) {
    todo!("draw_reactions: one line per reaction event with outcome icon")
}

/// Render the conversation list with selection highlighting.
fn draw_conversations(frame: &mut Frame, area: Rect, app: &App) {
    todo!("draw_conversations: list conv headers, highlight selected, truncate long titles")
}
