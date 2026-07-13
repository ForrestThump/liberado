//! Chat message pane keyboard handler (j/k navigate, Enter expand/collapse).

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect, Focus, Message};
use crate::tuning::PAGE_SCROLL_LINES;

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Tab | KeyCode::Esc => {
            app.focus = Focus::Input;
            vec![Effect::None]
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.chat_cursor + 1 < app.messages.len() {
                app.chat_cursor += 1;
            }
            app.scroll_to_chat_cursor();
            vec![Effect::None]
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.chat_cursor > 0 {
                app.chat_cursor -= 1;
            }
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
        // Fork from the message you're looking at — the reason you scrolled back here.
        KeyCode::Char('f') => fork_at_cursor(app),
        KeyCode::PageUp => {
            app.scroll_back(PAGE_SCROLL_LINES);
            vec![Effect::None]
        }
        KeyCode::PageDown => {
            app.scroll_forward(PAGE_SCROLL_LINES);
            vec![Effect::None]
        }
        _ => vec![Effect::None],
    }
}

/// Branch the conversation at the selected message, keeping the original.
///
/// This is what "browse back and fork from here" means: you scroll to the message you wish had gone
/// differently, press `f`, and continue from that point in a new conversation — the original is
/// still in `/sessions`, untouched.
fn fork_at_cursor(app: &mut App) -> Vec<Effect> {
    let Some(parent_id) = app.session.clone() else {
        app.messages
            .push(Message::System("No conversation to fork.".into()));
        return vec![Effect::None];
    };

    let Some(after_turn) = app.fork_turn_at_cursor() else {
        // Sitting on the first thing you ever said: there is no earlier context to branch to, so a
        // "fork" here would just be an empty conversation. Say that, rather than making one.
        app.messages.push(Message::System(
            "Nothing above this message to fork from — it's the start of the conversation. \
             Start a new chat instead."
                .into(),
        ));
        return vec![Effect::None];
    };

    app.messages.push(Message::System(format!(
        "Forking here — keeping your turns 1–{after_turn}. The original stays put."
    )));
    // Land in the branch: leave message-browsing so the input box is live in the new conversation.
    app.focus = Focus::Input;
    vec![Effect::ForkConversation {
        parent_id,
        after_turn: Some(after_turn),
    }]
}
