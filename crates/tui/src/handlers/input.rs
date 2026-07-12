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
    // Ghost-complete: Enter accepts the selected palette match (no Tab required).
    let message =
        if liberado_commands::is_slash_prefix(&app.input) && !app.slash_matches().is_empty() {
            liberado_commands::accept_completion(&app.input, app.slash_palette_index)
                .unwrap_or_else(|| app.input.clone())
                .trim()
                .to_string()
        } else {
            app.input.trim().to_string()
        };
    if message.is_empty() {
        return vec![Effect::None];
    }
    if message.starts_with('/') {
        return app.handle_slash_command(&message);
    }

    // Routed input (unified-Session model): when focused on a live goal session, the message is a
    // human reply delivered via `POST /api/goals/{id}/message`. We don't echo a local `User`
    // message — the session's stream echoes it back as `human_input`, keeping the transcript the
    // single source of truth (correct on rejoin too).
    if let Some(id) = app.input_target_session() {
        if let Some(j) = app.joined.as_mut() {
            j.awaiting = None; // optimistic: the prompt is answered
        }
        app.input.clear();
        app.cursor = 0;
        app.input_scroll = 0;
        app.scroll_offset = 0;
        return vec![Effect::SendGoalMessage { id, text: message }];
    }

    // Not routed to a session. If a *finished* session view is still showing, sending a chat
    // message auto-returns to the primary chat (the plan's auto-return-on-next-message).
    app.joined = None;

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
    app.clamp_slash_palette();
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
    app.clamp_slash_palette();
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
    let matches = app.slash_matches();
    if !matches.is_empty() {
        if app.slash_palette_index > 0 {
            app.slash_palette_index -= 1;
        }
        return vec![Effect::None];
    }
    let line = app.cursor_visual_line();
    if line == 0 {
        return vec![Effect::None];
    }
    let col = app.cursor_visual_col();
    app.cursor = app.byte_offset_for_visual(line - 1, col);
    vec![Effect::None]
}

fn handle_down(app: &mut App) -> Vec<Effect> {
    let matches = app.slash_matches();
    if !matches.is_empty() {
        if app.slash_palette_index + 1 < matches.len() {
            app.slash_palette_index += 1;
        }
        return vec![Effect::None];
    }
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
    // Progressive slash completion takes Tab when a palette is open.
    if let Some(completed) =
        liberado_commands::complete_commands(&app.input, app.slash_palette_index)
    {
        app.input = completed;
        app.cursor = app.input.len();
        app.clamp_slash_palette();
        return vec![Effect::None];
    }
    // Input ↔ chat history (no always-on conversation sidebar). Disabled while joined to a goal
    // session — that view is a read-only transcript, not the navigable conversation history.
    if app.joined.is_some() {
        return vec![Effect::None];
    }
    app.focus = Focus::ChatMessages;
    if app.chat_cursor >= app.messages.len() {
        app.chat_cursor = app.messages.len().saturating_sub(1);
    }
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
