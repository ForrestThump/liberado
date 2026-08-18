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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ConvHeader;
    use crate::render::test_support;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn conv(id: &str, title: &str) -> ConvHeader {
        ConvHeader {
            id: id.into(),
            title: Some(title.into()),
            created_at: "2025-06-25T12:00:00Z".into(),
            parent_conversation: None,
            spawned_by: None,
        }
    }

    fn browser_app() -> App {
        let mut app = test_support::app();
        app.conversations = vec![conv("c1", "one"), conv("c2", "two"), conv("c3", "three")];
        app.open_session_browser();
        app
    }

    #[test]
    fn navigation_moves_from_nonzero_and_clamps() {
        let mut app = browser_app();
        app.sidebar_selection = 2;
        handle(&mut app, key(KeyCode::Up));
        assert_eq!(app.sidebar_selection, 1, "Up moves down a row");
        handle(&mut app, key(KeyCode::Down));
        handle(&mut app, key(KeyCode::Down));
        assert_eq!(app.sidebar_selection, 2, "Down clamps at the last row");
        handle(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.sidebar_selection, 1, "k navigates with an empty filter");
        handle(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.sidebar_selection, 2, "j navigates with an empty filter");
    }

    #[test]
    fn vim_guards_yield_to_typing_when_filtering() {
        // With a filter set and a non-zero selection, `j`/`k` must become filter characters
        // (resetting the selection), not navigate. A guard turned `true` would navigate instead.
        let mut app = browser_app();
        app.sidebar_filter = "t".into();
        app.sidebar_selection = 2;
        handle(&mut app, key(KeyCode::Char('k')));
        assert_eq!(
            app.sidebar_selection, 0,
            "k typed the filter, not navigated"
        );
        assert_eq!(app.sidebar_filter, "tk");

        let mut app = browser_app();
        app.sidebar_filter = "t".into();
        app.sidebar_selection = 2;
        handle(&mut app, key(KeyCode::Char('j')));
        assert_eq!(
            app.sidebar_selection, 0,
            "j typed the filter, not navigated"
        );
    }

    #[test]
    fn new_conversation_clears_and_requests_a_refresh() {
        let mut app = browser_app();
        app.session = Some("c1".into());
        app.messages.push(Message::User("previous chat".into()));
        let effects = handle(&mut app, key(KeyCode::Char('n')));
        assert!(app.session.is_none(), "starts a fresh chat");
        assert_eq!(
            app.messages.len(),
            1,
            "only the fresh-chat system line remains"
        );
        assert!(
            matches!(&app.messages[0], Message::System(t) if t.contains("New conversation")),
            "starts with a fresh-chat notice: {:?}",
            app.messages
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::RefreshConversations)),
            "fresh list fetched"
        );
        assert_eq!(app.focus, Focus::Input, "leaves the browser");
    }

    #[test]
    fn new_conversation_is_a_filter_char_while_filtering() {
        let mut app = browser_app();
        app.sidebar_filter = "t".into();
        handle(&mut app, key(KeyCode::Char('n')));
        assert_eq!(app.sidebar_filter, "tn", "'n' is typed, not a jump");
    }

    #[test]
    fn backspace_pops_and_char_pushes_the_filter() {
        let mut app = browser_app();
        handle(&mut app, key(KeyCode::Char('a')));
        handle(&mut app, key(KeyCode::Char('b')));
        handle(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.sidebar_filter, "a");
        let mut app = browser_app();
        handle(&mut app, key(KeyCode::Backspace));
        assert!(
            app.sidebar_filter.is_empty(),
            "backspace on empty is a no-op"
        );
    }

    #[test]
    fn enter_opens_the_selected_conversation() {
        let mut app = browser_app();
        app.sidebar_selection = 1;
        let effects = handle(&mut app, key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        assert!(matches!(&effects[0], Effect::LoadConversationHistory(id) if id == "c2"));
        assert_eq!(app.pending_load.as_deref(), Some("c2"));
        assert_eq!(app.focus, Focus::Input);
        assert!(app.sidebar_filter.is_empty(), "filter cleared on open");
    }

    #[test]
    fn esc_closes_the_browser() {
        let mut app = browser_app();
        let effects = handle(&mut app, key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Input);
        assert!(effects.iter().any(|e| matches!(e, Effect::None)));
    }
}
