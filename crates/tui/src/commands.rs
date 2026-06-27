//! Slash-command handlers for the Liberado TUI.
//!
//! Each command is a free function `fn cmd_*(&mut App) -> Vec<Effect>`. The dispatch
//! table in `dispatch()` maps command strings to handlers. This module is separated
//! from `app.rs` to keep the App struct focused on state management.

use crate::app::{App, Effect, Message};
use crate::format::format_uptime;
use crate::tuning::CTX_PCT_DISPLAY_CAP;

pub fn dispatch(app: &mut App, input: &str) -> Vec<Effect> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    app.input.clear();
    app.cursor = 0;
    match cmd {
        "/quit" | "/exit" => vec![Effect::Quit],
        "/new" => cmd_new(app),
        "/clear" => cmd_clear(app),
        "/help" => cmd_help(app),
        "/status" => cmd_status(app),
        "/theme" => cmd_theme(
            app,
            parts.get(1).copied().unwrap_or(""),
            parts.get(2).copied(),
        ),
        "/model" => cmd_model(app),
        "/session" => cmd_session(
            app,
            parts.get(1).copied().unwrap_or(""),
            parts.get(2).copied(),
        ),
        "/fork" => cmd_fork(app),
        _ => cmd_unknown(app, cmd),
    }
}

fn cmd_unknown(app: &mut App, cmd: &str) -> Vec<Effect> {
    app.messages.push(Message::System(format!(
        "Unknown command: {cmd}. Type /help for available commands."
    )));
    app.scroll_offset = 0;
    vec![Effect::None]
}

fn cmd_new(app: &mut App) -> Vec<Effect> {
    let was_streaming = app.streaming;
    app.session = None;
    app.pending_load = None;
    app.collapsed_nodes.clear();
    app.messages.clear();
    app.chat_cursor = 0;
    app.expanded_messages.clear();
    app.assistant_buf.clear();
    app.streaming = false;
    app.scroll_offset = 0;
    app.focus = crate::app::Focus::Input;
    let mut effects = vec![Effect::RefreshConversations];
    if was_streaming {
        effects.push(Effect::CancelStream);
    }
    effects
}

fn cmd_clear(app: &mut App) -> Vec<Effect> {
    app.messages.clear();
    app.chat_cursor = 0;
    app.expanded_messages.clear();
    app.assistant_buf.clear();
    app.scroll_offset = 0;
    vec![Effect::None]
}

fn cmd_help(app: &mut App) -> Vec<Effect> {
    app.messages.push(Message::System(
        "\
Slash commands:
  /quit       quit the TUI
  /exit       quit the TUI (alias)
  /new        start a new conversation
  /clear      clear the chat display (local only)
  /help       show this help
  /status     show daemon connection info
  /session    session control (info, list, switch, close)
  /theme      theme switching (list, set, reload)
  /model      show model info
  /fork       fork current conversation (server support pending)

Keybindings:
  Enter       send message (Shift+Enter for newline)
  Ctrl+C      clear input, or quit when empty (press twice to exit)
  Ctrl+S      stop streaming (keep partial response)
  Tab         switch focus between input and sidebar
  Esc         clear input / cancel stream / return focus
  PgUp/PgDn   scroll chat
  j / k       navigate sidebar conversations
  Space       toggle tree fold (on sidebar parent nodes)
  n           new conversation (when sidebar focused)
  ← →         move cursor in input
  Home / End  jump to start/end of input
  Del         delete character after cursor"
            .into(),
    ));
    app.scroll_offset = 0;
    vec![Effect::None]
}

fn cmd_status(app: &mut App) -> Vec<Effect> {
    if let Some(ref st) = app.status {
        let model = st.model_name.as_deref().unwrap_or("(unknown)");
        let tokens = st
            .token_usage_total
            .map(|t| t.to_string())
            .unwrap_or_else(|| "--".into());
        let window = st
            .context_window
            .map(|w| w.to_string())
            .unwrap_or_else(|| "--".into());
        let fill = match (st.token_usage_total, st.context_window) {
            (Some(u), Some(w)) if w > 0 => format!(
                "{}%",
                (u as f64 / w as f64 * 100.0).min(CTX_PCT_DISPLAY_CAP) as u32
            ),
            _ => "--".to_string(),
        };
        let info = format!(
            "Daemon:  {} running\nVault:   {}\nUptime:  {}\nModel:   {model}\nTokens:  {tokens} / {window}  ({fill} context)\nDispatcher:    {}\nOrchestrator:  {}\nReactions seen: {}",
            crate::format::state_label(st.running),
            st.vault_path,
            format_uptime(st.uptime_seconds),
            crate::format::attached_label(st.dispatcher_attached),
            crate::format::attached_label(st.orchestrator_attached),
            st.reactions_seen,
        );
        app.messages.push(Message::System(info));
        app.scroll_offset = 0;
        vec![Effect::None]
    } else {
        app.messages.push(Message::System(
            "Not connected to daemon — waiting for status poll...".into(),
        ));
        app.scroll_offset = 0;
        vec![Effect::None]
    }
}

fn cmd_theme(app: &mut App, arg: &str, extra: Option<&str>) -> Vec<Effect> {
    match arg {
        "reload" => {
            if let Some(dir) = liberado_theme::user_themes_dir() {
                let errors = app.theme_registry.reload(&dir);
                for e in &errors {
                    app.messages
                        .push(Message::System(format!("theme error: {e}")));
                    app.scroll_offset = 0;
                }
                if errors.is_empty() {
                    let count = app.theme_registry.len();
                    app.messages.push(Message::System(format!(
                        "Themes reloaded — {count} available"
                    )));
                    app.scroll_offset = 0;
                    vec![Effect::None]
                } else {
                    vec![Effect::None]
                }
            } else {
                app.messages.push(Message::System(
                    "Could not determine theme directory".into(),
                ));
                app.scroll_offset = 0;
                vec![Effect::None]
            }
        }
        "" | "list" => {
            let names = app.theme_registry.names();
            let current = &app.theme.name;
            let lines: Vec<String> = names
                .iter()
                .map(|n| {
                    if *n == current.as_str() {
                        format!("  * {n}  (active)")
                    } else {
                        format!("    {n}")
                    }
                })
                .collect();
            app.messages.push(Message::System(format!(
                "Available themes:\n{}\n\nUsage: /theme set <name>  |  /theme reload",
                lines.join("\n")
            )));
            app.scroll_offset = 0;
            vec![Effect::None]
        }
        "set" => {
            let name = extra.unwrap_or("");
            if name.is_empty() {
                app.messages.push(Message::System(
                    "Usage: /theme set <name>\nUse /theme list to see available themes".into(),
                ));
                app.scroll_offset = 0;
                vec![Effect::None]
            } else if let Some(theme) = app.theme_registry.get(name).cloned() {
                app.theme = theme;
                app.messages.push(Message::System(format!("Theme: {name}")));
                app.scroll_offset = 0;
                vec![Effect::None]
            } else {
                let names = app.theme_registry.names();
                app.messages.push(Message::System(format!(
                    "Unknown theme: {name}. Available: {}",
                    names.join(", ")
                )));
                app.scroll_offset = 0;
                vec![Effect::None]
            }
        }
        _ => {
            if let Some(theme) = app.theme_registry.get(arg).cloned() {
                app.theme = theme;
                app.messages.push(Message::System(format!("Theme: {arg}")));
                app.scroll_offset = 0;
                vec![Effect::None]
            } else {
                let names = app.theme_registry.names();
                app.messages.push(Message::System(format!("Unknown theme: {arg}. Available: {}\nUsage: /theme set <name>  |  /theme list  |  /theme reload", names.join(", "))));
                app.scroll_offset = 0;
                vec![Effect::None]
            }
        }
    }
}

fn cmd_model(app: &mut App) -> Vec<Effect> {
    if let Some(ref st) = app.status {
        if let Some(ref model) = st.model_name {
            let tokens = st
                .token_usage_total
                .map(|t| t.to_string())
                .unwrap_or_else(|| "--".into());
            let window = st
                .context_window
                .map(|w| w.to_string())
                .unwrap_or_else(|| "--".into());
            app.messages.push(Message::System(format!(
                "Model: {model}\nTokens used: {tokens} / {window}"
            )));
            app.scroll_offset = 0;
            vec![Effect::None]
        } else {
            app.messages.push(Message::System("Model is configured server-side at daemon start.\nThe server has not yet exposed the model name via /api/status.".into()));
            app.scroll_offset = 0;
            vec![Effect::None]
        }
    } else {
        app.messages.push(Message::System(
            "Not connected to daemon.\nModel is configured server-side at daemon start.".into(),
        ));
        app.scroll_offset = 0;
        vec![Effect::None]
    }
}

fn cmd_session(app: &mut App, sub: &str, extra: Option<&str>) -> Vec<Effect> {
    match sub {
        "close" => {
            let id = app.session.take();
            app.assistant_buf.clear();
            app.streaming = false;
            if let Some(id) = id {
                app.messages.push(Message::System(format!("Closed session {id}. Messages preserved locally.\nUse /session switch <id> or sidebar to resume.")));
                app.scroll_offset = 0;
                vec![Effect::None]
            } else {
                app.messages
                    .push(Message::System("No active session to close.".into()));
                app.scroll_offset = 0;
                vec![Effect::None]
            }
        }
        "switch" => {
            let id = extra.unwrap_or("");
            if id.is_empty() {
                app.messages.push(Message::System("Usage: /session switch <session-id>\nThe id can be the full id or the first few characters seen in the sidebar.".into()));
                app.scroll_offset = 0;
                vec![Effect::None]
            } else {
                let match_id = app
                    .conversations
                    .iter()
                    .find(|c| c.id.starts_with(id))
                    .map(|c| c.id.clone())
                    .unwrap_or_else(|| id.to_string());
                app.pending_load = Some(match_id.clone());
                app.scroll_offset = 0;
                vec![Effect::LoadConversationHistory(match_id)]
            }
        }
        "list" | "" => {
            let session_str = app
                .session
                .as_deref()
                .map(|id| format!("Active session: {id}"))
                .unwrap_or_else(|| "No active session".into());
            app.messages.push(Message::System(format!("{session_str}\n{} conversations in list.\n\nCommands:\n  /session info          show current session details\n  /session list          show this message\n  /session switch <id>   load a conversation by id\n  /session close         detach from current session", app.conversations.len())));
            app.scroll_offset = 0;
            vec![Effect::None]
        }
        "info" => {
            if let Some(ref id) = app.session {
                let conv = app.conversations.iter().find(|c| c.id == *id);
                let title = conv
                    .and_then(|c| {
                        if c.title.is_empty() {
                            None
                        } else {
                            Some(c.title.clone())
                        }
                    })
                    .unwrap_or_else(|| "(untitled)".into());
                let lineage = conv
                    .and_then(|c| c.parent_conversation.as_deref())
                    .map(|p| format!("Forked from: {p}"))
                    .unwrap_or_else(|| "Root conversation".into());
                app.messages.push(Message::System(format!(
                    "Session: {id}\nTitle:   {title}\nMessages: {}\n{lineage}",
                    app.messages.len()
                )));
                app.scroll_offset = 0;
                vec![Effect::None]
            } else {
                app.messages.push(Message::System("No active session.\nUse /session switch <id> or select a conversation in the sidebar.".into()));
                app.scroll_offset = 0;
                vec![Effect::None]
            }
        }
        _ => {
            app.messages.push(Message::System(format!(
                "Unknown session command: {sub}\nTry: /session info | list | switch <id> | close"
            )));
            app.scroll_offset = 0;
            vec![Effect::None]
        }
    }
}

fn cmd_fork(app: &mut App) -> Vec<Effect> {
    if let Some(ref id) = app.session {
        app.messages.push(Message::System(
            format!("Forking from {id}…\nServer-side fork support is not yet available. The DAG visualization is ready."),
        ));
        app.scroll_offset = 0;
        vec![Effect::ForkConversation(id.clone())]
    } else {
        app.messages.push(Message::System("No active session to fork.\nUse /fork to branch a new conversation from the current one.".into()));
        app.scroll_offset = 0;
        vec![Effect::None]
    }
}
