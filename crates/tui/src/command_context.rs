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
        self.theme_registry
            .names()
            .into_iter()
            .map(String::from)
            .collect()
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
            // Persist for next launch — but only when a real settings path is set. Tests leave it
            // `None` so the suite never writes to (and clobbers) the user's config `settings.toml`.
            if let Some(path) = self.settings_path.clone()
                && let Err(e) = liberado_theme::save_theme_preference_to(&path, name)
            {
                tracing::warn!(error = %e, theme = %name, "failed to persist theme preference");
                self.push_system_message(format!("Theme: {name} (could not save preference: {e})"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ConvHeader;
    use crate::render::test_support;

    fn conv(id: &str, title: Option<&str>, parent: Option<&str>) -> ConvHeader {
        ConvHeader {
            id: id.into(),
            title: title.map(str::to_string),
            created_at: "2025-06-25T12:00:00Z".into(),
            parent_conversation: parent.map(str::to_string),
            spawned_by: None,
        }
    }

    fn context_app() -> App {
        let mut app = test_support::app();
        app.conversations = vec![
            conv("c1", Some("weekly planning"), None),
            conv("c2", Some(""), Some("c1")),
            conv("c3", None, None),
        ];
        app
    }

    #[test]
    fn session_and_stream_probes_read_app_state() {
        let mut app = context_app();
        assert_eq!(app.active_session_id(), None);
        assert!(!app.is_streaming());
        app.session = Some("s1".into());
        app.streaming = true;
        assert_eq!(app.active_session_id(), Some("s1"));
        assert!(app.is_streaming());
        assert_eq!(app.conversation_count(), 3);
        assert_eq!(app.message_count(), 0);
        app.messages.push(Message::User("hi".into()));
        assert_eq!(app.message_count(), 1);
    }

    #[test]
    fn prefix_lookup_finds_or_misses() {
        let app = context_app();
        assert_eq!(app.find_conversation_id_by_prefix("c1"), Some("c1".into()));
        assert_eq!(app.find_conversation_id_by_prefix("zz"), None);
    }

    #[test]
    fn status_info_requires_a_live_status() {
        let app = context_app();
        assert!(app.status_info().is_none());
        let mut app = context_app();
        app.status = Some(crate::api::DaemonStatus {
            running: true,
            vault_path: "/v".into(),
            uptime_seconds: 9,
            watcher_active: false,
            dispatcher_attached: false,
            orchestrator_attached: false,
            reactions_seen: 0,
            model_name: Some("m".into()),
            token_usage_total: Some(10),
            context_window: Some(100),
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            enter_sends: true,
        });
        let info = app.status_info().unwrap();
        assert!(info.running);
        assert_eq!(info.vault_path, "/v");
        assert_eq!(info.model_name.as_deref(), Some("m"));
        assert_eq!(info.uptime_seconds, 9);
    }

    #[test]
    fn theme_names_and_current_theme_read_the_registry() {
        let app = context_app();
        let names = app.theme_names();
        assert!(!names.is_empty(), "registry ships built-ins");
        assert!(
            app.current_theme_name().is_empty()
                || names.contains(&app.current_theme_name().to_string()),
            "current theme must be one of the registry names: {names:?}"
        );
    }

    #[test]
    fn conversation_queries_by_exact_id() {
        let app = context_app();
        // c1 has a title; c2's empty title must fall back to None.
        assert_eq!(
            app.conversation_title_for("c1"),
            Some("weekly planning".into())
        );
        assert_eq!(app.conversation_title_for("c2"), None);
        assert_eq!(app.conversation_title_for("missing"), None);
        // Parent lookup must be by exact id — a `==`→`!=` mutation would answer c1 here.
        assert_eq!(app.conversation_parent_for("missing"), None);
        assert_eq!(app.conversation_parent_for("c2"), Some("c1".into()));
        assert_eq!(app.conversation_parent_for("c3"), None);
    }

    #[test]
    fn conversation_list_flattens_titles_and_ids() {
        let app = context_app();
        let list = app.conversation_list();
        assert_eq!(list.len(), 3);
        assert!(list.contains(&("weekly planning".to_string(), "c1".to_string())));
        assert!(
            list.contains(&(String::new(), "c2".to_string())),
            "empty titles pass through as-is: {list:?}"
        );
        assert!(
            list.contains(&("(untitled)".to_string(), "c3".to_string())),
            "missing titles fall back to (untitled): {list:?}"
        );
    }

    #[test]
    fn lifecycle_mutators_change_state() {
        let mut app = context_app();
        app.set_active_session(Some("x".into()));
        assert_eq!(app.session.as_deref(), Some("x"));
        app.push_system_message("note".into());
        assert_eq!(app.messages.len(), 1);
        app.clear_input();
        assert!(app.input.is_empty());
        app.streaming = true;
        app.assistant_buf = "partial".into();
        app.stop_streaming();
        assert!(!app.streaming);
        assert!(app.assistant_buf.is_empty(), "partial buffer dropped");
        app.messages.push(Message::User("hi".into()));
        app.clear_chat();
        assert!(app.messages.is_empty(), "clear_chat empties the transcript");
        app.session = Some("s".into());
        app.streaming = true;
        app.messages.push(Message::User("hi".into()));
        app.reset_for_new_conversation();
        assert!(app.session.is_none());
        assert!(!app.streaming);
        assert!(app.messages.is_empty());
    }

    #[test]
    fn set_theme_accepts_registry_names_and_rejects_unknowns() {
        let mut app = context_app();
        let names = app.theme_names();
        let first = names.first().cloned().unwrap_or_else(|| "dark".into());
        let changed = app.set_theme(&first);
        assert!(changed, "a registry theme must apply");
        assert_eq!(app.current_theme_name(), first);
        assert!(!app.set_theme("no-such-theme"), "unknown theme rejected");
    }
}
