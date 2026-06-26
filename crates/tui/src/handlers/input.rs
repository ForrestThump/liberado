//! Text input keyboard handler.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Effect, Focus, Message};
use crate::tuning::PAGE_SCROLL_LINES;

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                app.input.insert(app.cursor, '\n');
                app.cursor += 1;
                return vec![Effect::None];
            }
            let message = app.input.trim().to_string();
            if message.is_empty() {
                return vec![Effect::None];
            }
            if message.starts_with('/') {
                return app.handle_slash_command(&message);
            }
            if app.streaming {
                return vec![Effect::None];
            }
            let session = app.session.clone();
            app.messages.push(Message::User(message.clone()));
            app.input.clear();
            app.cursor = 0;
            app.streaming = true;
            app.assistant_buf.clear();
            app.scroll_offset = 0;
            vec![Effect::StartChatStream { message, session }]
        }
        KeyCode::Char(c) => { app.input.insert(app.cursor, c); app.cursor += 1; vec![Effect::None] }
        KeyCode::Backspace => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let boundary = crate::app::prev_word_boundary(&app.input, app.cursor);
                for _ in boundary..app.cursor { app.input.remove(boundary); }
                app.cursor = boundary;
                return vec![Effect::None];
            }
            if app.cursor > 0 { app.cursor -= 1; app.input.remove(app.cursor); }
            vec![Effect::None]
        }
        KeyCode::Delete => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let boundary = crate::app::next_word_boundary(&app.input, app.cursor);
                for _ in app.cursor..boundary { app.input.remove(app.cursor); }
                while app.cursor < app.input.len() && app.input.as_bytes()[app.cursor].is_ascii_whitespace() {
                    app.input.remove(app.cursor);
                }
                return vec![Effect::None];
            }
            if app.cursor < app.input.len() { app.input.remove(app.cursor); }
            vec![Effect::None]
        }
        KeyCode::Left => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                app.cursor = crate::app::prev_word_boundary(&app.input, app.cursor);
                return vec![Effect::None];
            }
            if app.cursor > 0 { app.cursor -= 1; }
            vec![Effect::None]
        }
        KeyCode::Right => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                app.cursor = crate::app::next_word_boundary(&app.input, app.cursor);
                return vec![Effect::None];
            }
            if app.cursor < app.input.len() { app.cursor += 1; }
            vec![Effect::None]
        }
        KeyCode::Home => { app.cursor = 0; vec![Effect::None] }
        KeyCode::End => { app.cursor = app.input.len(); vec![Effect::None] }
        KeyCode::Esc => {
            if app.streaming { return app.cancel_stream(); }
            app.input.clear();
            app.cursor = 0;
            vec![Effect::None]
        }
        KeyCode::Tab => { app.focus = Focus::SidebarConversations; vec![Effect::None] }
        KeyCode::PageUp => { app.scroll_back(PAGE_SCROLL_LINES); vec![Effect::None] }
        KeyCode::PageDown => { app.scroll_forward(PAGE_SCROLL_LINES); vec![Effect::None] }
        _ => vec![Effect::None],
    }
}
