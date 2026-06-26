//! Chat message pane keyboard handler (j/k navigate, Enter expand/collapse).

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect, Focus};
use crate::tuning::PAGE_SCROLL_LINES;

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Tab | KeyCode::Esc => { app.focus = Focus::Input; vec![Effect::None] }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.chat_cursor + 1 < app.messages.len() { app.chat_cursor += 1; }
            app.scroll_to_chat_cursor();
            vec![Effect::None]
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.chat_cursor > 0 { app.chat_cursor -= 1; }
            app.scroll_to_chat_cursor();
            vec![Effect::None]
        }
        KeyCode::Enter => {
            if app.chat_cursor < app.messages.len() {
                if app.expanded_messages.contains(&app.chat_cursor) {
                    app.expanded_messages.remove(&app.chat_cursor);
                } else {
                    app.expanded_messages.insert(app.chat_cursor);
                }
            }
            vec![Effect::None]
        }
        KeyCode::PageUp => { app.scroll_back(PAGE_SCROLL_LINES); vec![Effect::None] }
        KeyCode::PageDown => { app.scroll_forward(PAGE_SCROLL_LINES); vec![Effect::None] }
        _ => vec![Effect::None],
    }
}
