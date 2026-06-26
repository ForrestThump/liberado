//! Mouse event handler for click and scroll in all panes.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::{App, Effect, Focus};
use crate::tuning::MOUSE_SCROLL_LINES;

/// Returns `true` if the point at `(col, row)` lies inside rectangle `r`.
fn point_in_rect(col: u16, row: u16, r: Rect) -> bool {
    col >= r.x && col < r.x.saturating_add(r.width)
        && row >= r.y && row < r.y.saturating_add(r.height)
}

pub(crate) fn handle(app: &mut App, event: MouseEvent) -> Vec<Effect> {
    let (col, row) = (event.column, event.row);
    let chat = app.layout.chat;
    let sidebar_full = app.layout.sidebar_full;
    let sidebar = app.layout.sidebar_conversations;
    let input_rect = app.layout.input;

    match event.kind {
        MouseEventKind::ScrollDown => {
            if point_in_rect(col, row, chat) { app.scroll_back(MOUSE_SCROLL_LINES); }
            else if point_in_rect(col, row, sidebar_full) {
                let visible = app.visible_conversations();
                if app.sidebar_selection + 1 < visible.len() { app.sidebar_selection += 1; }
            }
            vec![Effect::None]
        }
        MouseEventKind::ScrollUp => {
            if point_in_rect(col, row, chat) { app.scroll_forward(MOUSE_SCROLL_LINES); }
            else if point_in_rect(col, row, sidebar_full) && app.sidebar_selection > 0 { app.sidebar_selection -= 1; }
            vec![Effect::None]
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if point_in_rect(col, row, input_rect) {
                if col > input_rect.x { app.cursor = (col - input_rect.x - 1) as usize; }
                app.cursor = app.cursor.min(app.input.len());
                app.focus = Focus::Input;
                return vec![Effect::None];
            }
            if point_in_rect(col, row, sidebar) {
                app.focus = Focus::SidebarConversations;
                let item_row = row.saturating_sub(sidebar.y + 1);
                let item_idx = item_row as usize;
                let visible = app.visible_conversations();
                if item_idx < visible.len() {
                    let prev = app.sidebar_selection;
                    app.sidebar_selection = item_idx;
                    if item_idx == prev {
                        let node = &visible[item_idx];
                        if node.has_children {
                            if node.collapsed { app.collapsed_nodes.remove(&node.header.id); }
                            else { app.collapsed_nodes.insert(node.header.id.clone()); }
                            return vec![Effect::None];
                        }
                        let id = node.header.id.clone();
                        app.pending_load = Some(id.clone());
                        app.sidebar_filter.clear();
                        return vec![Effect::LoadConversationHistory(id)];
                    }
                }
                return vec![Effect::None];
            }
            if point_in_rect(col, row, sidebar_full) && !point_in_rect(col, row, sidebar) {
                app.focus = Focus::SidebarConversations;
                return vec![Effect::None];
            }
            if point_in_rect(col, row, chat) {
                app.focus = Focus::ChatMessages;
                app.chat_cursor = app.messages.len().saturating_sub(1);
                return vec![Effect::None];
            }
            vec![Effect::None]
        }
        _ => vec![Effect::None],
    }
}
