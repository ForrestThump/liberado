//! Unified session switcher keyboard handler (`Focus::SessionSwitcher`, opened by `/sessions`).
//!
//! Row 0 is always the **primary chat** (selecting it returns focus there — the `/back`
//! affordance); rows 1.. are goal sessions filtered by the type-ahead. Enter on a goal row
//! `/join`s it.

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

/// Enter on the selected row: row 0 = primary chat (leave any joined session); rows 1.. = join the
/// corresponding goal session.
fn open_selected(app: &mut App) -> Vec<Effect> {
    if app.sidebar_selection == 0 {
        // Primary chat row: return focus there, dropping any joined session.
        app.close_session_switcher();
        if app.joined.is_some() {
            app.leave_session();
            return vec![Effect::LeaveGoalSession];
        }
        return vec![Effect::None];
    }

    let idx = app.sidebar_selection - 1;
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
