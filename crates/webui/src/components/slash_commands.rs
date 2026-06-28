use liberado_commands::{CommandContext, CommandResult, StatusInfo};

use super::chat::ChatMsg;

struct WebCommandContext {
    pub messages: Vec<ChatMsg>,
    pub session_id: Option<String>,
}

impl CommandContext for WebCommandContext {
    fn active_session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    fn is_streaming(&self) -> bool {
        false
    }
    fn conversation_count(&self) -> usize {
        0
    }
    fn find_conversation_id_by_prefix(&self, _prefix: &str) -> Option<String> {
        None
    }
    fn status_info(&self) -> Option<StatusInfo> {
        None
    }
    fn theme_names(&self) -> Vec<String> {
        Vec::new()
    }
    fn current_theme_name(&self) -> &str {
        "dark"
    }
    fn conversation_title_for(&self, _id: &str) -> Option<String> {
        None
    }
    fn conversation_parent_for(&self, _id: &str) -> Option<String> {
        None
    }
    fn message_count(&self) -> usize {
        0
    }
    fn conversation_list(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn set_active_session(&mut self, id: Option<String>) {
        self.session_id = id;
    }
    fn clear_chat(&mut self) {
        self.messages.clear();
    }
    fn reset_for_new_conversation(&mut self) {
        self.session_id = None;
        self.messages.clear();
    }
    fn push_system_message(&mut self, msg: String) {
        self.messages.push(ChatMsg {
            role: "system",
            content: msg,
        });
    }
    fn clear_input(&mut self) {}
    fn stop_streaming(&mut self) {}
    fn set_theme(&mut self, _name: &str) -> bool {
        false
    }
    fn reload_themes(&mut self) -> Result<usize, Vec<String>> {
        Ok(0)
    }
}

pub fn handle_slash_command(text: &str) -> (Vec<ChatMsg>, Option<String>, Vec<CommandResult>) {
    let cmd = match liberado_commands::parse(text) {
        Some(c) => c,
        None => {
            let msg = ChatMsg {
                role: "system",
                content: format!("Unknown command: {text}. Type /help for available commands."),
            };
            return (vec![msg], None, Vec::new());
        }
    };

    let mut ctx = WebCommandContext {
        messages: Vec::new(),
        session_id: None,
    };

    let results = liberado_commands::dispatch(&cmd, &mut ctx);
    (ctx.messages, ctx.session_id, results)
}
