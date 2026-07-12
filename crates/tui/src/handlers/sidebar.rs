//! Full-screen session browser keyboard handler (`Focus::SessionBrowser`).

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect, Focus, Message};

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc => {
            app.close_session_browser();
            vec![Effect::None]
        }
        KeyCode::Up => {
            if app.sidebar_selection > 0 {
                app.sidebar_selection -= 1;
            }
            vec![Effect::None]
        }
        KeyCode::Down => {
            let n = app.visible_conversations().len();
            if app.sidebar_selection + 1 < n {
                app.sidebar_selection += 1;
            }
            vec![Effect::None]
        }
        // Vim-style nav only when the filter is empty so typing "task" still works.
        KeyCode::Char('k') if app.sidebar_filter.is_empty() => {
            if app.sidebar_selection > 0 {
                app.sidebar_selection -= 1;
            }
            vec![Effect::None]
        }
        KeyCode::Char('j') if app.sidebar_filter.is_empty() => {
            let n = app.visible_conversations().len();
            if app.sidebar_selection + 1 < n {
                app.sidebar_selection += 1;
            }
            vec![Effect::None]
        }
        KeyCode::Char('n') if app.sidebar_filter.is_empty() => {
            app.close_session_browser();
            app.session = None;
            app.messages.clear();
            app.chat_cursor = 0;
            app.expanded_messages.clear();
            app.assistant_buf.clear();
            app.pending_load = None;
            app.messages.push(Message::System(
                "New conversation — send a message to start a fresh session.".into(),
            ));
            vec![Effect::RefreshConversations]
        }
        KeyCode::Enter => open_selected(app),
        KeyCode::Backspace => {
            if !app.sidebar_filter.is_empty() {
                app.sidebar_filter.pop();
                app.sidebar_selection = 0;
                app.clamp_sidebar_selection();
            }
            vec![Effect::None]
        }
        KeyCode::Char(c) => {
            app.sidebar_filter.push(c);
            app.sidebar_selection = 0;
            vec![Effect::None]
        }
        _ => vec![Effect::None],
    }
}

fn open_selected(app: &mut App) -> Vec<Effect> {
    let visible = app.visible_conversations();
    let Some(node) = visible.get(app.sidebar_selection) else {
        return vec![Effect::None];
    };
    let id = node.header.id.clone();
    app.pending_load = Some(id.clone());
    app.sidebar_filter.clear();
    app.focus = Focus::Input;
    vec![Effect::LoadConversationHistory(id)]
}
