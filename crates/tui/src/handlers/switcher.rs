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

/// Enter on the selected row. Conversation rows come first (switch the primary chat to that
/// conversation, leaving any joined goal session); goal rows follow (`/join` the session).
fn open_selected(app: &mut App) -> Vec<Effect> {
    let conv_count = app.filtered_conversations().len();
    let sel = app.sidebar_selection;

    if sel < conv_count {
        // Conversation row → make it the active primary chat.
        let convs = app.filtered_conversations();
        let Some(header) = convs.get(sel) else {
            return vec![Effect::None];
        };
        let id = header.id.clone();
        let already_active = app.session.as_deref() == Some(id.as_str());
        drop(convs);

        app.close_session_switcher();
        let mut effects = Vec::new();
        // Selecting a conversation always returns you to the primary chat surface.
        if app.joined.is_some() {
            app.leave_session();
            effects.push(Effect::LeaveGoalSession);
        }
        if already_active {
            // Already on this conversation — just came back to it (no reload needed).
            if effects.is_empty() {
                effects.push(Effect::None);
            }
            return effects;
        }
        app.pending_load = Some(id.clone());
        effects.push(Effect::LoadConversationHistory(id));
        return effects;
    }

    // Goal-session row → focus (`/join`) it.
    let idx = sel - conv_count;
    let filtered = app.filtered_goal_sessions();
    let Some(header) = filtered.get(idx) else {
        return vec![Effect::None];
    };
    let id = header.id.clone();
    drop(filtered);
    app.close_session_switcher();
    app.join_session(id.clone());
    vec![Effect::JoinGoalSession(id)]
}
