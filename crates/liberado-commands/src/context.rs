#[derive(Debug, Clone)]
pub struct StatusInfo {
    pub running: bool,
    pub vault_path: String,
    pub uptime_seconds: u64,
    pub model_name: Option<String>,
    pub token_usage_total: Option<u64>,
    pub context_window: Option<u64>,
    pub dispatcher_attached: bool,
    pub orchestrator_attached: bool,
    pub reactions_seen: u64,
}

pub trait CommandContext {
    fn active_session_id(&self) -> Option<&str>;
    fn is_streaming(&self) -> bool;
    fn conversation_count(&self) -> usize;
    fn find_conversation_id_by_prefix(&self, prefix: &str) -> Option<String>;
    fn status_info(&self) -> Option<StatusInfo>;
    fn theme_names(&self) -> Vec<String>;
    fn current_theme_name(&self) -> &str;
    fn conversation_title_for(&self, id: &str) -> Option<String>;
    fn conversation_parent_for(&self, id: &str) -> Option<String>;
    fn message_count(&self) -> usize;
    fn conversation_list(&self) -> Vec<(String, String)>;

    fn set_active_session(&mut self, id: Option<String>);
    fn clear_chat(&mut self);
    fn reset_for_new_conversation(&mut self);
    fn push_system_message(&mut self, msg: String);
    fn clear_input(&mut self);
    fn stop_streaming(&mut self);
    fn set_theme(&mut self, name: &str) -> bool;
    fn reload_themes(&mut self) -> Result<usize, Vec<String>>;
}
