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
