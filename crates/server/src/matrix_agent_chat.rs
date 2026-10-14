//! Slash commands for one agent's Matrix DM.
//!
//! The room is pinned to that agent's existing session. Commands that would
//! open, switch, or fork a conversation are answered here and never reach the
//! shared bridge, because the bridge would move the binding.

use async_trait::async_trait;
use liberado_messaging::ChatSurface;

use crate::telegram::TextChatBridge;

/// [`TextChatBridge`] that refuses to leave this agent.
pub struct PinnedAgentChat {
    inner: TextChatBridge,
}

impl PinnedAgentChat {
    pub fn new(inner: TextChatBridge) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl ChatSurface for PinnedAgentChat {
    async fn reply(&self, user_text: &str) -> Result<String, String> {
        let text = user_text.trim();
        if let Some(answer) = dm_command_answer(text) {
            return Ok(answer);
        }
        self.inner.reply(text).await
    }
}

/// Menu advertised for an agent DM. The main room keeps the shared menu.
pub fn dm_command_menu() -> Vec<(String, String)> {
    vec![
        ("help".into(), "commands for this agent".into()),
        ("status".into(), "this chat".into()),
        ("model".into(), "show or set the model for this chat".into()),
        ("stop".into(), "cancel the current turn".into()),
    ]
}

fn dm_command_answer(text: &str) -> Option<String> {
    if !text.starts_with('/') {
        return None;
    }
    let word = command_word(text);
    if word == "help" {
        return Some(dm_help());
    }
    skipped_reason(&word).map(str::to_string)
}

fn command_word(text: &str) -> String {
    text.split_whitespace()
        .next()
        .unwrap_or(text)
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

fn skipped_reason(word: &str) -> Option<&'static str> {
    match word {
        "new" | "session" | "sessions" => {
            Some("This chat stays on this agent. Open another agent from the Agents shelf.")
        }
        "fork" => Some("This chat stays on this agent, so /fork is not available here."),
        "join" | "spawn" | "back" | "goal" | "plan" | "explore" => {
            Some("Goal commands are not available in an agent DM.")
        }
        "quit" | "exit" | "clear" | "theme" => {
            Some("That command needs a local client. It does nothing in this DM.")
        }
        "profile" => Some("This agent's profile is its shelf identity. Change it in the WebUI."),
        _ => None,
    }
}

fn dm_help() -> String {
    "\
Commands in this agent chat:
  /help — this list
  /status — this chat
  /model — show or set the model for this chat
  /stop — cancel the current turn
  /cancel — same as /stop"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{dm_command_answer, dm_help};

    #[test]
    fn help_lists_only_the_commands_this_room_runs() {
        let help = dm_command_answer("/help").expect("help");
        assert_eq!(help, dm_help());
        for allowed in ["/status", "/model", "/stop", "/cancel"] {
            assert!(help.contains(allowed), "{allowed} missing from {help}");
        }
        for skipped in ["/new", "/fork", "/sessions", "/spawn", "/goal", "/profile"] {
            assert!(!help.contains(skipped), "{skipped} listed in {help}");
        }
    }

    #[test]
    fn chat_and_the_pinned_commands_reach_the_bridge() {
        assert_eq!(dm_command_answer("status of the build"), None);
        assert_eq!(dm_command_answer("/status"), None);
        assert_eq!(dm_command_answer("/model grok"), None);
        assert_eq!(dm_command_answer("/stop"), None);
        assert_eq!(dm_command_answer("/cancel"), None);
    }

    #[test]
    fn switching_and_local_commands_stay_here() {
        let pinned = dm_command_answer("/new").expect("new");
        assert!(pinned.contains("Agents shelf"));
        assert_eq!(dm_command_answer("/session"), Some(pinned.clone()));
        assert_eq!(dm_command_answer("/sessions"), Some(pinned));
        assert!(
            dm_command_answer("/fork 2")
                .expect("fork")
                .contains("/fork")
        );
        let goal = dm_command_answer("/join abc").expect("join");
        for word in ["/spawn", "/back", "/goal", "/plan", "/explore"] {
            assert_eq!(dm_command_answer(word).as_deref(), Some(goal.as_str()));
        }
        let local = dm_command_answer("/quit").expect("quit");
        for word in ["/exit", "/clear", "/theme"] {
            assert_eq!(dm_command_answer(word).as_deref(), Some(local.as_str()));
        }
        assert!(
            dm_command_answer("/profile")
                .expect("profile")
                .contains("WebUI")
        );
    }
}
