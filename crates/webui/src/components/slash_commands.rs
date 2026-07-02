use chat_client_contract::{ConvHeader, DaemonStatus};
use liberado_commands::{CommandContext, CommandResult, SlashCommand, StatusInfo};

use super::chat::ChatMsg;

/// Snapshot of the chat state a slash command needs to read, plus somewhere to record the
/// mutations handlers make (`ctx.push_system_message`, `ctx.set_active_session`, ...). The caller
/// (`chat.rs`) applies `messages`/`session_id` back onto the real Dioxus signals after dispatch —
/// `CommandContext`'s `&self` read methods can't return borrows tied to a `Signal`'s transient
/// `read()` guard, so a snapshot-then-reapply is the shape the trait requires.
struct WebCommandContext {
    messages: Vec<ChatMsg>,
    session_id: Option<String>,
    sending: bool,
    message_count: usize,
    conversations: Vec<ConvHeader>,
    status: Option<DaemonStatus>,
}

impl CommandContext for WebCommandContext {
    fn active_session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    fn is_streaming(&self) -> bool {
        self.sending
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
        vec!["dark".to_string()]
    }
    fn current_theme_name(&self) -> &str {
        "dark"
    }
    fn conversation_title_for(&self, id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.title.clone())
            .filter(|t| !t.is_empty())
    }
    fn conversation_parent_for(&self, _id: &str) -> Option<String> {
        // `ConvHeader` doesn't carry lineage over the wire today (only the DB/conversation-store
        // side does) — honestly report "unknown" rather than guessing.
        None
    }
    fn message_count(&self) -> usize {
        self.message_count
    }
    fn conversation_list(&self) -> Vec<(String, String)> {
        self.conversations
            .iter()
            .map(|c| {
                let title = c
                    .title
                    .clone()
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "(untitled)".into());
                (title, c.id.clone())
            })
            .collect()
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
            thinking_steps: Vec::new(),
        });
    }
    fn clear_input(&mut self) {}
    fn stop_streaming(&mut self) {}
    fn set_theme(&mut self, _name: &str) -> bool {
        false
    }
    fn reload_themes(&mut self) -> Result<usize, Vec<String>> {
        Err(vec![
            "Theme switching isn't implemented in the web UI yet.".into(),
        ])
    }
}

async fn fetch_conversations(api_base: &str) -> Vec<ConvHeader> {
    let url = format!("{api_base}/api/conversations");
    match reqwest::get(&url).await {
        Ok(resp) => resp.json::<Vec<ConvHeader>>().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

async fn fetch_status(api_base: &str) -> Option<DaemonStatus> {
    let url = format!("{api_base}/api/status");
    let resp = reqwest::get(&url).await.ok()?;
    resp.json::<DaemonStatus>().await.ok()
}

fn parse_error(text: &str) -> (Vec<ChatMsg>, Option<String>, Vec<CommandResult>) {
    let msg = ChatMsg {
        role: "system",
        content: format!("Unknown command: {text}. Type /help for available commands."),
        thinking_steps: Vec::new(),
    };
    (vec![msg], None, Vec::new())
}

/// Render a `ShowOptions` result (from `/theme list`, `/session list`) as a plain system message —
/// the web UI has no modal/picker widget yet, so a numbered list is the honest fallback.
fn render_options(title: &str, options: &[(String, String)]) -> String {
    let mut out = format!("{title}:\n");
    for (label, _id) in options {
        out.push_str("  ");
        out.push_str(label);
        out.push('\n');
    }
    out
}

/// Run a slash command against a snapshot of the current chat state. Fetches the conversation list
/// or daemon status over HTTP only when the command actually needs it (`/session ...`, `/status`,
/// `/model`), so `/new`, `/clear`, `/help`, etc. stay instant with no network round trip.
pub async fn handle_slash_command(
    text: &str,
    api_base: &str,
    session_id: Option<String>,
    sending: bool,
    message_count: usize,
) -> (Vec<ChatMsg>, Option<String>, Vec<CommandResult>) {
    let cmd = match liberado_commands::parse(text) {
        Some(c) => c,
        None => return parse_error(text),
    };

    let (conversations, status) = match &cmd {
        SlashCommand::Session(_) => (fetch_conversations(api_base).await, None),
        SlashCommand::Status | SlashCommand::Model => (Vec::new(), fetch_status(api_base).await),
        _ => (Vec::new(), None),
    };

    let mut ctx = WebCommandContext {
        messages: Vec::new(),
        session_id,
        sending,
        message_count,
        conversations,
        status,
    };

    let results = liberado_commands::dispatch(&cmd, &mut ctx);
    for result in &results {
        if let CommandResult::ShowOptions { title, options } = result {
            ctx.push_system_message(render_options(title, options));
        }
    }
    (ctx.messages, ctx.session_id, results)
}
