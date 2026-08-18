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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Effect;
    use crate::render::test_support;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn app_with_models() -> App {
        let mut app = test_support::app();
        app.models = vec!["alpha".into(), "beta".into(), "gamma".into()];
        app
    }

    #[test]
    fn navigation_stays_in_bounds_and_moves_from_nonzero() {
        let mut app = app_with_models();
        app.sidebar_selection = 0;
        handle(&mut app, key(KeyCode::Up));
        assert_eq!(app.sidebar_selection, 0, "Up at the top stays");
        // Move from a non-zero position so a deleted Up/`k` arm or a flipped guard shows.
        app.sidebar_selection = 2;
        handle(&mut app, key(KeyCode::Up));
        assert_eq!(app.sidebar_selection, 1, "Up moves down a row");
        handle(&mut app, key(KeyCode::Down));
        handle(&mut app, key(KeyCode::Down));
        assert_eq!(app.sidebar_selection, 2, "Down clamps at the last model");
        handle(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.sidebar_selection, 2, "j at the bottom stays");
        handle(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.sidebar_selection, 1, "k moves up");
        // And from the top, j/k move the other way.
        app.sidebar_selection = 0;
        handle(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.sidebar_selection, 1, "j moves down from the top");
        handle(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.sidebar_selection, 0, "k moves back up");
        // k at the very top must stay: a > -> >= mutation underflows.
        handle(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.sidebar_selection, 0, "k at the top stays");
    }

    #[test]
    fn vim_nav_is_disabled_while_filtering() {
        let mut app = app_with_models();
        app.sidebar_filter = "be".into();
        app.sidebar_selection = 2;
        handle(&mut app, key(KeyCode::Char('k')));
        assert_eq!(
            app.sidebar_selection, 0,
            "k is a filter char, not navigation, when filtering"
        );
        assert_eq!(app.sidebar_filter, "bek");
        let mut app = app_with_models();
        app.sidebar_filter = "be".into();
        app.sidebar_selection = 2;
        handle(&mut app, key(KeyCode::Char('j')));
        assert_eq!(
            app.sidebar_selection, 0,
            "j is a filter char, not navigation, when filtering"
        );
    }

    #[test]
    fn type_backspace_and_char_drive_the_filter() {
        let mut app = app_with_models();
        handle(&mut app, key(KeyCode::Char('a')));
        assert_eq!(app.sidebar_filter, "a");
        handle(&mut app, key(KeyCode::Char('l')));
        assert_eq!(app.sidebar_filter, "al");
        handle(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.sidebar_filter, "a");
        // A backspace with an empty filter is a no-op, not a panic.
        let mut app = app_with_models();
        handle(&mut app, key(KeyCode::Backspace));
        assert!(app.sidebar_filter.is_empty());
    }

    #[test]
    fn refresh_requests_a_fetch() {
        let mut app = app_with_models();
        let effects = handle(&mut app, key(KeyCode::Char('r')));
        assert!(app.models_loading);
        assert!(app.models_error.is_none());
        assert!(effects.iter().any(|e| matches!(e, Effect::FetchModels)));
    }

    #[test]
    fn enter_selects_the_filtered_row_for_the_conversation() {
        let mut app = app_with_models();
        app.sidebar_filter = "ga".into(); // → ["gamma"]
        app.session = Some("c1".into());
        let effects = handle(&mut app, key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        assert!(
            matches!(&effects[0], Effect::SelectModel { model, conversation }
            if model == "gamma" && conversation == &Some("c1".into()))
        );
        let system = app
            .messages
            .iter()
            .find(|m| matches!(m, Message::System(_)))
            .expect("a status line");
        let Message::System(text) = system else {
            unreachable!()
        };
        assert!(
            text.contains("Setting model for this conversation"),
            "{text}"
        );
    }

    #[test]
    fn already_active_model_closes_without_an_effect() {
        let mut app = app_with_models();
        app.status = Some(crate::api::DaemonStatus {
            running: true,
            vault_path: "/v".into(),
            uptime_seconds: 0,
            watcher_active: false,
            dispatcher_attached: false,
            orchestrator_attached: false,
            reactions_seen: 0,
            model_name: Some("alpha".into()),
            token_usage_total: None,
            context_window: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            enter_sends: true,
        });
        // No conversation open + the current model is selected → nothing to do.
        let effects = handle(&mut app, key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::None));
        assert_eq!(app.focus, crate::app::Focus::Input, "browser closes");
    }
}
