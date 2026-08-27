//! A compact [`CommandContext`] double shared by the survivor-test modules.
//!
//! `tests.rs` owns a richer mock for its own suites; this one carries just the
//! state the survivor assertions observe: emitted messages, cleared input,
//! theme bookkeeping, and the optional status snapshot.

use crate::context::{CommandContext, StatusInfo};
use std::collections::HashMap;

pub(crate) struct SurvivorCtx {
    pub(crate) session: Option<String>,
    pub(crate) streaming: bool,
    pub(crate) conversations: Vec<(String, String, Option<String>)>,
    pub(crate) status: Option<StatusInfo>,
    pub(crate) themes: Vec<String>,
    pub(crate) current_theme: String,
    pub(crate) messages: Vec<String>,
    pub(crate) input_cleared: bool,
    pub(crate) set_results: HashMap<String, bool>,
    pub(crate) reload: Result<usize, Vec<String>>,
}

impl SurvivorCtx {
    pub(crate) fn new() -> Self {
        Self {
            session: None,
            streaming: false,
            conversations: Vec::new(),
            status: None,
            themes: vec!["dark".into(), "light".into()],
            current_theme: "dark".into(),
            messages: Vec::new(),
            input_cleared: false,
            set_results: HashMap::new(),
            reload: Ok(2),
        }
    }

    pub(crate) fn with_status(status: StatusInfo) -> Self {
        let mut ctx = Self::new();
        ctx.status = Some(status);
        ctx
    }

    pub(crate) fn last_message(&self) -> &str {
        self.messages.last().map(String::as_str).unwrap_or("")
    }
}

impl Default for SurvivorCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandContext for SurvivorCtx {
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
            .find(|(id, _, _)| id.starts_with(prefix))
            .map(|(id, _, _)| id.clone())
    }
    fn status_info(&self) -> Option<StatusInfo> {
        self.status.clone()
    }
    fn theme_names(&self) -> Vec<String> {
        self.themes.clone()
    }
    fn current_theme_name(&self) -> &str {
        &self.current_theme
    }
    fn conversation_title_for(&self, id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|(cid, _, _)| cid == id)
            .map(|(_, title, _)| title.clone())
    }
    fn conversation_parent_for(&self, id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|(cid, _, _)| cid == id)
            .and_then(|(_, _, parent)| parent.clone())
    }
    fn message_count(&self) -> usize {
        self.messages.len()
    }
    fn conversation_list(&self) -> Vec<(String, String)> {
        self.conversations
            .iter()
            .map(|(id, title, _)| (title.clone(), id.clone()))
            .collect()
    }

    fn set_active_session(&mut self, id: Option<String>) {
        self.session = id;
    }
    fn clear_chat(&mut self) {
        self.messages.clear();
    }
    fn reset_for_new_conversation(&mut self) {
        self.session = None;
        self.messages.clear();
        self.streaming = false;
    }
    fn push_system_message(&mut self, msg: String) {
        self.messages.push(msg);
    }
    fn clear_input(&mut self) {
        self.input_cleared = true;
    }
    fn stop_streaming(&mut self) {
        self.streaming = false;
    }
    fn set_theme(&mut self, name: &str) -> bool {
        if let Some(&result) = self.set_results.get(name) {
            result
        } else {
            self.themes.iter().any(|t| t == name)
        }
    }
    fn reload_themes(&mut self) -> Result<usize, Vec<String>> {
        self.reload.clone()
    }
}
