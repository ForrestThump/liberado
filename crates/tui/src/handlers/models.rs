//! Full-screen model browser keyboard handler (`Focus::ModelBrowser`).

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect, Message};

pub(crate) fn handle(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc => {
            app.close_model_browser();
            vec![Effect::None]
        }
        KeyCode::Up => {
            if app.sidebar_selection > 0 {
                app.sidebar_selection -= 1;
            }
            vec![Effect::None]
        }
        KeyCode::Down => {
            let n = app.filtered_models().len();
            if app.sidebar_selection + 1 < n {
                app.sidebar_selection += 1;
            }
            vec![Effect::None]
        }
        KeyCode::Char('k') if app.sidebar_filter.is_empty() => {
            if app.sidebar_selection > 0 {
                app.sidebar_selection -= 1;
            }
            vec![Effect::None]
        }
        KeyCode::Char('j') if app.sidebar_filter.is_empty() => {
            let n = app.filtered_models().len();
            if app.sidebar_selection + 1 < n {
                app.sidebar_selection += 1;
            }
            vec![Effect::None]
        }
        KeyCode::Char('r') if app.sidebar_filter.is_empty() => {
            app.models_loading = true;
            app.models_error = None;
            vec![Effect::FetchModels]
        }
        KeyCode::Enter => select_model(app),
        KeyCode::Backspace => {
            if !app.sidebar_filter.is_empty() {
                app.sidebar_filter.pop();
                app.sidebar_selection = 0;
                app.clamp_model_selection();
            }
            vec![Effect::None]
        }
        KeyCode::Char(c) => {
            app.sidebar_filter.push(c);
            app.sidebar_selection = 0;
            app.clamp_model_selection();
            vec![Effect::None]
        }
        _ => vec![Effect::None],
    }
}

fn select_model(app: &mut App) -> Vec<Effect> {
    let filtered: Vec<String> = app.filtered_models().into_iter().cloned().collect();
    let Some(name) = filtered.get(app.sidebar_selection).cloned() else {
        return vec![Effect::None];
    };
    // Scope to the open conversation when one is selected (WebUI contract); otherwise
    // daemon-wide — two meanings of the same command depending on whether a chat is open.
    let conversation = app.session.clone();
    let current = app
        .status
        .as_ref()
        .and_then(|s| s.model_name.clone())
        .unwrap_or_else(|| "(unknown)".into());
    // Daemon-wide "already active" only applies when not scoping to a conversation — a chat
    // may want a model that differs from the process default.
    if conversation.is_none() && name == current {
        app.close_model_browser();
        app.messages
            .push(Message::System(format!("Model: {name}  (already active)")));
        app.scroll_offset = 0;
        return vec![Effect::None];
    }
    if conversation.is_some() {
        app.messages.push(Message::System(format!(
            "Setting model for this conversation to `{name}`…"
        )));
    } else {
        app.messages.push(Message::System(format!(
            "Switching active model to `{name}`…"
        )));
    }
    app.scroll_offset = 0;
    vec![Effect::SelectModel {
        model: name,
        conversation,
    }]
}
