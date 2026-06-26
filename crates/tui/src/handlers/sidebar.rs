//! Sidebar conversation list keyboard handler.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect, Focus};
use crate::tuning::PAGE_SCROLL_LINES;

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc => { app.sidebar_filter.clear(); app.clamp_sidebar_selection(); app.focus = Focus::Input; vec![Effect::None] }
        KeyCode::Tab => { app.sidebar_filter.clear(); app.clamp_sidebar_selection(); app.focus = Focus::ChatMessages; app.chat_cursor = app.messages.len().saturating_sub(1); vec![Effect::None] }
        KeyCode::Backspace => { if !app.sidebar_filter.is_empty() { app.sidebar_filter.pop(); app.sidebar_selection = 0; } vec![Effect::None] }
        KeyCode::Up | KeyCode::Char('k') => { if app.sidebar_selection > 0 { app.sidebar_selection -= 1; } vec![Effect::None] }
        KeyCode::Down | KeyCode::Char('j') => {
            let visible = app.visible_conversations();
            if app.sidebar_selection + 1 < visible.len() { app.sidebar_selection += 1; }
            vec![Effect::None]
        }
        KeyCode::Enter => {
            let visible = app.visible_conversations();
            if let Some(node) = visible.get(app.sidebar_selection) && node.has_children {
                if node.collapsed { app.collapsed_nodes.remove(&node.header.id); }
                else { app.collapsed_nodes.insert(node.header.id.clone()); }
                return vec![Effect::None];
            }
            if let Some(node) = visible.get(app.sidebar_selection) {
                let id = node.header.id.clone();
                app.pending_load = Some(id.clone());
                app.sidebar_filter.clear();
                app.focus = Focus::Input;
                vec![Effect::LoadConversationHistory(id)]
            } else { vec![Effect::None] }
        }
        KeyCode::Char(' ') => {
            let visible = app.visible_conversations();
            if let Some(node) = visible.get(app.sidebar_selection) && node.has_children {
                if node.collapsed { app.collapsed_nodes.remove(&node.header.id); }
                else { app.collapsed_nodes.insert(node.header.id.clone()); }
                return vec![Effect::None];
            }
            app.sidebar_filter.push(' ');
            app.sidebar_selection = 0;
            vec![Effect::None]
        }
        KeyCode::Char('n') => {
            if !app.sidebar_filter.is_empty() { app.sidebar_filter.push('n'); app.sidebar_selection = 0; return vec![Effect::None]; }
            app.session = None; app.pending_load = None; app.collapsed_nodes.clear(); app.messages.clear(); app.chat_cursor = 0; app.expanded_messages.clear(); app.assistant_buf.clear();
            app.input.clear(); app.cursor = 0; app.scroll_offset = 0;
            app.focus = Focus::Input;
            vec![Effect::RefreshConversations]
        }
        KeyCode::Char(c) => {
            if c.is_alphanumeric() || c == '-' || c == '_' { app.sidebar_filter.push(c); app.sidebar_selection = 0; }
            vec![Effect::None]
        }
        KeyCode::PageUp => { app.scroll_back(PAGE_SCROLL_LINES); vec![Effect::None] }
        KeyCode::PageDown => { app.scroll_forward(PAGE_SCROLL_LINES); vec![Effect::None] }
        _ => vec![Effect::None],
    }
}
