use crate::context::CommandContext;
use crate::result::CommandResult;

pub fn handle(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    if let Some(ref id) = ctx.active_session_id().map(String::from) {
        ctx.push_system_message(format!(
            "Forking from {id}…\nServer-side fork support is not yet available. The DAG visualization is ready."
        ));
        vec![CommandResult::ForkRequested {
            parent_id: id.clone(),
        }]
    } else {
        ctx.push_system_message(
            "No active session to fork.\nUse /fork to branch a new conversation from the current one.".into(),
        );
        vec![CommandResult::None]
    }
}
