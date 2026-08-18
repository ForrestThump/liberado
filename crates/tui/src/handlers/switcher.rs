//! Unified session switcher keyboard handler (`Focus::SessionSwitcher`, opened by `/session` and
//! `/sessions`).
//!
//! One flat, filterable list: prior conversations (primary chats) first, then goal sessions. Enter
//! on a conversation switches the primary chat to it (leaving any joined goal session); Enter on a
//! goal row `/join`s it.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect};

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc => {
            app.close_session_switcher();
            vec![Effect::None]
        }
        KeyCode::Up => {
            if app.sidebar_selection > 0 {
                app.sidebar_selection -= 1;
            }
            vec![Effect::None]
        }
        KeyCode::Down => {
            if app.sidebar_selection + 1 < app.switcher_row_count() {
                app.sidebar_selection += 1;
            }
            vec![Effect::None]
        }
        // Vim nav only when the filter is empty, so typing a query still works.
        KeyCode::Char('k') if app.sidebar_filter.is_empty() => {
            if app.sidebar_selection > 0 {
                app.sidebar_selection -= 1;
            }
            vec![Effect::None]
        }
        KeyCode::Char('j') if app.sidebar_filter.is_empty() => {
            if app.sidebar_selection + 1 < app.switcher_row_count() {
                app.sidebar_selection += 1;
            }
            vec![Effect::None]
        }
        KeyCode::Enter => open_selected(app),
        KeyCode::Backspace => {
            if !app.sidebar_filter.is_empty() {
                app.sidebar_filter.pop();
                app.sidebar_selection = 0;
                app.clamp_switcher_selection();
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

/// Enter on the selected row.
///
/// One list, and the branch is the one attribute that actually distinguishes a session (D7): a row
/// **with a goal** is joined (it runs to a terminal status and you watch it); a row **without one**
/// is a chat, so it becomes the active conversation. No row-type bookkeeping, no index arithmetic
/// across two lists — the store's own distinction is the UI's.
fn open_selected(app: &mut App) -> Vec<Effect> {
    let sel = app.sidebar_selection;

    let (id, has_goal) = {
        let sessions = app.filtered_sessions();
        let Some(s) = sessions.get(sel) else {
            return vec![Effect::None];
        };
        (s.id.clone(), s.has_goal())
    };

    if has_goal {
        app.close_session_switcher();
        app.join_session(id.clone());
        return vec![Effect::JoinGoalSession(id)];
    }

    // A chat → make it the active conversation. Selecting one always returns you to the primary
    // surface, so any joined session is left behind first.
    let already_active = app.session.as_deref() == Some(id.as_str());
    app.close_session_switcher();
    let mut effects = Vec::new();
    if app.joined.is_some() {
        app.leave_session();
        effects.push(Effect::LeaveGoalSession);
    }
    if already_active {
        if effects.is_empty() {
            effects.push(Effect::None);
        }
        return effects;
    }
    app.pending_load = Some(id.clone());
    effects.push(Effect::LoadConversationHistory(id));
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Effect;
    use crate::render::test_support;
    use chat_client_contract::{DomainWire, SessionSummary};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn chat(id: &str, title: &str) -> SessionSummary {
        test_support::chat_session(id, title)
    }

    fn goal(id: &str, desc: &str) -> SessionSummary {
        test_support::goal_session(id, DomainWire::Coding, desc, "running", true)
    }

    #[test]
    fn esc_closes_the_switcher() {
        let mut app = test_support::app();
        app.sessions = vec![chat("c1", "t")];
        app.open_session_switcher();
        let effects = handle(&mut app, key(KeyCode::Esc));
        assert_eq!(app.focus, crate::app::Focus::Input);
        assert!(effects.iter().any(|e| matches!(e, Effect::None)));
    }

    #[test]
    fn navigation_and_filter_bound_the_selection() {
        let mut app = test_support::app();
        app.sessions = vec![chat("c1", "one"), chat("c2", "two"), goal("g1", "goal")];
        app.open_session_switcher();
        handle(&mut app, key(KeyCode::Up));
        assert_eq!(app.sidebar_selection, 0, "Up at top stays");
        // Move from a non-zero position so a deleted arm or flipped guard shows.
        app.sidebar_selection = 2;
        handle(&mut app, key(KeyCode::Up));
        assert_eq!(app.sidebar_selection, 1, "Up moves down a row");
        for _ in 0..5 {
            handle(&mut app, key(KeyCode::Down));
        }
        assert_eq!(app.sidebar_selection, 2, "Down clamps at last row");
        handle(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.sidebar_selection, 1, "k navigates with an empty filter");
        handle(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.sidebar_selection, 2, "j navigates with an empty filter");
        // With a filter set, j/k become filter characters, not navigation.
        app.sidebar_filter = "t".into();
        app.sidebar_selection = 2;
        handle(&mut app, key(KeyCode::Char('k')));
        assert_eq!(
            app.sidebar_selection, 0,
            "k is a filter char while filtering"
        );
        assert_eq!(app.sidebar_filter, "tk");
        handle(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.sidebar_filter, "t");
        let empty = test_support::app();
        let mut app = empty;
        handle(&mut app, key(KeyCode::Backspace));
        assert!(
            app.sidebar_filter.is_empty(),
            "backspace on empty is a no-op"
        );
    }

    #[test]
    fn enter_on_a_goal_row_joins_it() {
        let mut app = test_support::app();
        app.sessions = vec![goal("g1", "build the CLI")];
        app.open_session_switcher();
        let effects = handle(&mut app, key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        assert!(matches!(&effects[0], Effect::JoinGoalSession(id) if id == "g1"));
        assert!(app.joined.is_some(), "joined session is set");
        assert_eq!(app.focus, crate::app::Focus::Input, "switcher closes");
    }

    #[test]
    fn enter_on_a_chat_row_loads_it_as_the_active_conversation() {
        let mut app = test_support::app();
        app.sessions = vec![chat("c1", "one"), chat("c2", "two")];
        app.session = Some("c1".into());
        app.open_session_switcher();
        app.sidebar_selection = 1;
        let effects = handle(&mut app, key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        assert!(matches!(&effects[0], Effect::LoadConversationHistory(id) if id == "c2"));
        assert_eq!(app.pending_load.as_deref(), Some("c2"));
    }

    #[test]
    fn enter_on_the_already_active_chat_is_a_noop() {
        let mut app = test_support::app();
        app.sessions = vec![chat("c1", "one")];
        app.session = Some("c1".into());
        app.open_session_switcher();
        let effects = handle(&mut app, key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::None));
        assert_eq!(app.session.as_deref(), Some("c1"));
    }

    #[test]
    fn selecting_a_chat_leaves_a_joined_goal_session_first() {
        let mut app = test_support::app();
        app.sessions = vec![chat("c1", "one"), goal("g9", "goal")];
        // Currently joined to g9 and viewing it; the active primary chat is a *different* one,
        // so selecting c1 must both leave g9 and load c1.
        app.joined = Some(crate::app::JoinedSession {
            id: "g9".into(),
            kind: chat_client_contract::SessionKind::Coding,
            status: "running".into(),
            finished: false,
            description: "goal".into(),
            messages: Vec::new(),
            stream_buf: String::new(),
            awaiting: None,
            gate_votes: Vec::new(),
            active_role: None,
            last_validation: None,
        });
        app.session = Some("c0".into());
        app.open_session_switcher();
        app.sidebar_selection = 0; // the chat row
        let effects = handle(&mut app, key(KeyCode::Enter));
        assert_eq!(effects.len(), 2);
        assert!(matches!(effects[0], Effect::LeaveGoalSession));
        assert!(matches!(&effects[1], Effect::LoadConversationHistory(id) if id == "c1"));
        assert!(app.joined.is_none(), "leaves the joined session");
    }

    #[test]
    fn enter_with_an_empty_list_is_a_noop() {
        let mut app = test_support::app();
        app.open_session_switcher();
        let effects = handle(&mut app, key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::None));
    }
}
