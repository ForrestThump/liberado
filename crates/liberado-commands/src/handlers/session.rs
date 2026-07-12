use crate::commands::SessionCmd;
use crate::context::CommandContext;
use crate::result::CommandResult;

pub fn handle(cmd: &SessionCmd, ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    match cmd {
        SessionCmd::Close => {
            let id = ctx.active_session_id().map(String::from);
            ctx.stop_streaming();
            ctx.set_active_session(None);
            if let Some(ref id) = id {
                ctx.push_system_message(format!(
                    "Closed session {id}. Messages preserved on the server.\nUse /session to browse and resume."
                ));
                vec![CommandResult::SessionClosed {
                    id: Some(id.clone()),
                }]
            } else {
                ctx.push_system_message("No active session to close.".into());
                vec![CommandResult::SessionClosed { id: None }]
            }
        }
        SessionCmd::Switch(id) => {
            if id.is_empty() {
                ctx.push_system_message(
                    "Usage: /session switch <session-id>\nOr run /session to open the searchable browser.".into(),
                );
                vec![CommandResult::None]
            } else {
                let match_id = ctx
                    .find_conversation_id_by_prefix(id)
                    .unwrap_or_else(|| id.clone());
                vec![CommandResult::SessionSwitched { id: match_id }]
            }
        }
        SessionCmd::List => {
            // Surfaces open a full-screen browser; no chat dump.
            vec![CommandResult::OpenSessionBrowser]
        }
        SessionCmd::Info => {
            if let Some(ref id) = ctx.active_session_id().map(String::from) {
                let title = ctx
                    .conversation_title_for(id)
                    .unwrap_or_else(|| "(untitled)".into());
                let lineage = ctx
                    .conversation_parent_for(id)
                    .map(|p| format!("Forked from: {p}"))
                    .unwrap_or_else(|| "Root conversation".into());
                let msg_count = ctx.message_count();
                ctx.push_system_message(format!(
                    "Session: {id}\nTitle:   {title}\nMessages: {msg_count}\n{lineage}"
                ));
                vec![CommandResult::SessionInfoShown]
            } else {
                ctx.push_system_message(
                    "No active session.\nUse /session to browse prior conversations, or just chat to start one.".into(),
                );
                vec![CommandResult::None]
            }
        }
        SessionCmd::Unknown(sub) => {
            ctx.push_system_message(format!(
                "Unknown session command: {sub}\nTry: /session | list | info | switch <id> | close"
            ));
            vec![CommandResult::None]
        }
    }
}
