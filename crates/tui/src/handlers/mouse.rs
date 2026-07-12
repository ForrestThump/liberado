//! Mouse event handler for click and scroll.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::{App, Effect, Focus};
use crate::tuning::MOUSE_SCROLL_LINES;

fn point_in_rect(col: u16, row: u16, r: Rect) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

pub(crate) fn handle(app: &mut App, event: MouseEvent) -> Vec<Effect> {
    let (col, row) = (event.column, event.row);

    if app.focus == Focus::SessionBrowser {
        return handle_session_browser(app, event);
    }

    let chat = app.layout.chat;
    let input_rect = app.layout.input;

    match event.kind {
        MouseEventKind::ScrollDown => {
            if point_in_rect(col, row, chat) {
                app.scroll_back(MOUSE_SCROLL_LINES);
            }
            vec![Effect::None]
        }
        MouseEventKind::ScrollUp => {
            if point_in_rect(col, row, chat) {
                app.scroll_forward(MOUSE_SCROLL_LINES);
            }
            vec![Effect::None]
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if point_in_rect(col, row, input_rect) {
                if col > input_rect.x {
                    app.cursor = (col - input_rect.x - 1) as usize;
                }
                app.cursor = app.cursor.min(app.input.len());
                app.focus = Focus::Input;
                return vec![Effect::None];
            }
            if point_in_rect(col, row, chat) {
                app.focus = Focus::ChatMessages;
                // Approximate row → message index from scroll offset.
                let inner_row = row.saturating_sub(chat.y.saturating_add(1)) as usize;
                let idx = app.scroll_offset.saturating_add(inner_row);
                if idx < app.messages.len() {
                    app.chat_cursor = idx;
                } else {
                    app.chat_cursor = app.messages.len().saturating_sub(1);
                }
                return vec![Effect::None];
            }
            vec![Effect::None]
        }
        _ => vec![Effect::None],
    }
}

fn handle_session_browser(app: &mut App, event: MouseEvent) -> Vec<Effect> {
    let area = app.layout.session_browser;
    match event.kind {
        MouseEventKind::ScrollDown => {
            let n = app.visible_conversations().len();
            if app.sidebar_selection + 1 < n {
                app.sidebar_selection += 1;
            }
            vec![Effect::None]
        }
        MouseEventKind::ScrollUp => {
            if app.sidebar_selection > 0 {
                app.sidebar_selection -= 1;
            }
            vec![Effect::None]
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // List starts after the 3-row filter block + border.
            let list_top = area.y.saturating_add(4);
            if event.row >= list_top {
                let idx = (event.row - list_top) as usize;
                let visible = app.visible_conversations();
                if idx < visible.len() {
                    let prev = app.sidebar_selection;
                    app.sidebar_selection = idx;
                    if idx == prev {
                        let id = visible[idx].header.id.clone();
                        app.pending_load = Some(id.clone());
                        app.sidebar_filter.clear();
                        app.focus = Focus::Input;
                        return vec![Effect::LoadConversationHistory(id)];
                    }
                }
            }
            vec![Effect::None]
        }
        _ => vec![Effect::None],
    }
}
