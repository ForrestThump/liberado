use crate::context::CommandContext;
use crate::result::CommandResult;

pub fn handle(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    let was_streaming = ctx.is_streaming();
    ctx.set_active_session(None);
    ctx.reset_for_new_conversation();
    ctx.clear_input();
    vec![CommandResult::NewConversation { was_streaming }]
}
