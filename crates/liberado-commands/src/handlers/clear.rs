use crate::context::CommandContext;
use crate::result::CommandResult;

pub fn handle(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_chat();
    ctx.clear_input();
    vec![CommandResult::ChatCleared]
}
