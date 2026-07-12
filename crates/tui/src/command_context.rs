use liberado_commands::{CommandContext, StatusInfo};
use liberado_theme;

use crate::app::{App, Focus, Message};

impl CommandContext for App {
    fn active_session_id(&self) -> Option<&str> {
        self.session.as_deref()
    }

    fn is_streaming(&self) -> bool {
        self.streaming
    }

    fn conversation_count(&self) -> usize {
        self.conversations.len()
    }

    fn find_conversation_id_by_prefix(&self, prefix: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|c| c.id.starts_with(prefix))
            .map(|c| c.id.clone())
    }

    fn status_info(&self) -> Option<StatusInfo> {
        self.status.as_ref().map(|s| StatusInfo {
            running: s.running,
            vault_path: s.vault_path.clone(),
            uptime_seconds: s.uptime_seconds,
            model_name: s.model_name.clone(),
            token_usage_total: s.token_usage_total,
            context_window: s.context_window,
            dispatcher_attached: s.dispatcher_attached,
            orchestrator_attached: s.orchestrator_attached,
            reactions_seen: s.reactions_seen,
        })
    }

    fn theme_names(&self) -> Vec<String> {
        self.theme_registry.names().into_iter().map(String::from).collect()
    }

    fn current_theme_name(&self) -> &str {
        &self.theme.name
    }

    fn conversation_title_for(&self, id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.title.as_ref().filter(|t| !t.is_empty()).cloned())
    }

    fn conversation_parent_for(&self, id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.parent_conversation.clone())
    }

    fn message_count(&self) -> usize {
        self.messages.len()
    }

    fn conversation_list(&self) -> Vec<(String, String)> {
        self.conversations
            .iter()
            .map(|c| {
                let title = c.title.as_deref().unwrap_or("(untitled)");
                (title.to_string(), c.id.clone())
            })
            .collect()
    }

    fn set_active_session(&mut self, id: Option<String>) {
        self.session = id;
    }

    fn clear_chat(&mut self) {
        self.messages.clear();
        self.chat_cursor = 0;
        self.expanded_messages.clear();
        self.assistant_buf.clear();
        self.scroll_offset = 0;
    }

    fn reset_for_new_conversation(&mut self) {
        self.session = None;
        self.pending_load = None;
        self.collapsed_nodes.clear();
        self.messages.clear();
        self.chat_cursor = 0;
        self.expanded_messages.clear();
        self.assistant_buf.clear();
        self.streaming = false;
        self.scroll_offset = 0;
        self.focus = Focus::Input;
    }

    fn push_system_message(&mut self, msg: String) {
        self.messages.push(Message::System(msg));
        self.scroll_offset = 0;
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.input_scroll = 0;
    }

    fn stop_streaming(&mut self) {
        self.assistant_buf.clear();
        self.streaming = false;
    }

    fn set_theme(&mut self, name: &str) -> bool {
        if let Some(theme) = self.theme_registry.get(name).cloned() {
            self.theme = theme;
            // Persist for next launch (platform config `liberado/settings.toml`).
            if let Err(e) = liberado_theme::save_theme_preference(name) {
                tracing::warn!(error = %e, theme = %name, "failed to persist theme preference");
                self.push_system_message(format!(
                    "Theme: {name} (could not save preference: {e})"
                ));
            }
            true
        } else {
            false
        }
    }

    fn reload_themes(&mut self) -> Result<usize, Vec<String>> {
        if let Some(dir) = liberado_theme::user_themes_dir() {
            let errors = self.theme_registry.reload(&dir);
            let count = self.theme_registry.len();
            if errors.is_empty() {
                Ok(count)
            } else {
                Err(errors.into_iter().map(|e| e.to_string()).collect())
            }
        } else {
            Err(vec!["Could not determine theme directory".into()])
        }
    }
}
