//! Text input keyboard handler.
//!
//! Each key is handled by a dedicated function so the control flow stays
//! obvious and easy to test independently.
//!
//! * `handle_enter` / `handle_shift_enter` / `send_message`
//! * `handle_char` (with auto-wrap)
//! * `handle_backspace` / `handle_delete`
//! * `handle_left` / `handle_right` / `handle_home` / `handle_end`
//! * `handle_esc` / `handle_tab` / `handle_pgup` / `handle_pgdn`

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Effect, Focus, Message};
use crate::tuning::PAGE_SCROLL_LINES;

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let effects = match key.code {
        KeyCode::Enter => handle_enter(app, key.modifiers),
        KeyCode::Char(c) => handle_char(app, c),
        KeyCode::Backspace => handle_backspace(app, key.modifiers),
        KeyCode::Delete => handle_delete(app, key.modifiers),
        KeyCode::Left => handle_left(app, key.modifiers),
        KeyCode::Right => handle_right(app, key.modifiers),
        KeyCode::Home => handle_home(app),
        KeyCode::End => handle_end(app),
        KeyCode::Esc => handle_esc(app),
        KeyCode::Tab => handle_tab(app),
        KeyCode::Up => handle_up(app),
        KeyCode::Down => handle_down(app),
        KeyCode::PageUp => handle_pgup(app),
        KeyCode::PageDown => handle_pgdn(app),
        _ => vec![Effect::None],
    };
    app.scroll_input_to_cursor();
    effects
}

// ── Enter ────────────────────────────────────────────────────────────

fn handle_enter(app: &mut App, mods: KeyModifiers) -> Vec<Effect> {
    if mods.contains(KeyModifiers::SHIFT) {
        return handle_shift_enter(app);
    }
    send_message(app)
}

fn handle_shift_enter(app: &mut App) -> Vec<Effect> {
    app.input.insert(app.cursor, '\n');
    app.cursor += 1;
    vec![Effect::None]
}

fn send_message(app: &mut App) -> Vec<Effect> {
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
    app.input_scroll = 0;
    app.streaming = true;
    app.assistant_buf.clear();
    app.scroll_offset = 0;
    vec![Effect::StartChatStream { message, session }]
}

// ── Character ────────────────────────────────────────────────────────

fn handle_char(app: &mut App, c: char) -> Vec<Effect> {
    app.input.insert(app.cursor, c);
    app.cursor += 1;
    auto_wrap_if_needed(app);
    vec![Effect::None]
}

/// When the cursor passes the visible margin, break the current line at
/// the last word boundary (mimics what Shift+Enter would do).
fn auto_wrap_if_needed(app: &mut App) {
    let width = app.layout.input_content_width;
    if width == 0 || app.cursor_col() < width {
        return;
    }
    let line_start = app.input[..app.cursor]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let current = &app.input[line_start..app.cursor];
    if let Some(space) = current.rfind(' ') {
        app.input
            .replace_range((line_start + space)..(line_start + space + 1), "\n");
    }
}

// ── Deletion ─────────────────────────────────────────────────────────

fn handle_backspace(app: &mut App, mods: KeyModifiers) -> Vec<Effect> {
    if mods.contains(KeyModifiers::CONTROL) {
        let boundary = crate::app::prev_word_boundary(&app.input, app.cursor);
        for _ in boundary..app.cursor {
            app.input.remove(boundary);
        }
        app.cursor = boundary;
    } else if app.cursor > 0 {
        app.cursor -= 1;
        app.input.remove(app.cursor);
    }
    vec![Effect::None]
}

fn handle_delete(app: &mut App, mods: KeyModifiers) -> Vec<Effect> {
    if mods.contains(KeyModifiers::CONTROL) {
        let boundary = crate::app::next_word_boundary(&app.input, app.cursor);
        for _ in app.cursor..boundary {
            app.input.remove(app.cursor);
        }
        // Also eat trailing whitespace after the deleted word.
        while app.cursor < app.input.len() && app.input.as_bytes()[app.cursor].is_ascii_whitespace()
        {
            app.input.remove(app.cursor);
        }
    } else if app.cursor < app.input.len() {
        app.input.remove(app.cursor);
    }
    vec![Effect::None]
}

// ── Movement ─────────────────────────────────────────────────────────

fn handle_left(app: &mut App, mods: KeyModifiers) -> Vec<Effect> {
    if mods.contains(KeyModifiers::CONTROL) {
        app.cursor = crate::app::prev_word_boundary(&app.input, app.cursor);
    } else if app.cursor > 0 {
        app.cursor -= 1;
    }
    vec![Effect::None]
}

fn handle_right(app: &mut App, mods: KeyModifiers) -> Vec<Effect> {
    if mods.contains(KeyModifiers::CONTROL) {
        app.cursor = crate::app::next_word_boundary(&app.input, app.cursor);
    } else if app.cursor < app.input.len() {
        app.cursor += 1;
    }
    vec![Effect::None]
}

fn handle_home(app: &mut App) -> Vec<Effect> {
    app.cursor = 0;
    vec![Effect::None]
}

fn handle_end(app: &mut App) -> Vec<Effect> {
    app.cursor = app.input.len();
    vec![Effect::None]
}

fn handle_up(app: &mut App) -> Vec<Effect> {
    let line = app.cursor_visual_line();
    if line == 0 {
        return vec![Effect::None];
    }
    let col = app.cursor_visual_col();
    app.cursor = app.byte_offset_for_visual(line - 1, col);
    vec![Effect::None]
}

fn handle_down(app: &mut App) -> Vec<Effect> {
    let total = app.input_visual_lines();
    let line = app.cursor_visual_line();
    if line + 1 >= total {
        return vec![Effect::None];
    }
    let col = app.cursor_visual_col();
    app.cursor = app.byte_offset_for_visual(line + 1, col);
    vec![Effect::None]
}

// ── Misc keys ────────────────────────────────────────────────────────

fn handle_esc(app: &mut App) -> Vec<Effect> {
    if app.streaming {
        return app.cancel_stream();
    }
    app.input.clear();
    app.cursor = 0;
    app.input_scroll = 0;
    vec![Effect::None]
}

fn handle_tab(app: &mut App) -> Vec<Effect> {
    app.focus = Focus::SidebarConversations;
    vec![Effect::None]
}

fn handle_pgup(app: &mut App) -> Vec<Effect> {
    app.scroll_back(PAGE_SCROLL_LINES);
    vec![Effect::None]
}

fn handle_pgdn(app: &mut App) -> Vec<Effect> {
    app.scroll_forward(PAGE_SCROLL_LINES);
    vec![Effect::None]
}
