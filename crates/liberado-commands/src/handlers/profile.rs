use crate::context::CommandContext;
use crate::result::CommandResult;

/// Open the session-profile picker for the current chat.
///
/// Unlike `/model`, which hot-swaps the daemon's model for everyone, a profile is **per
/// conversation**: it decides which tools this chat may call and whether it may dispatch. Switching
/// is a human-only act — nothing in any tool catalog reaches it — and takes effect on the next turn.
pub fn handle(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    ctx.push_system_message(
        "Opening session profiles — type to filter, Enter to switch, Esc to close.
         A profile sets the tools this conversation may use; it applies from your next message."
            .into(),
    );
    vec![
        CommandResult::ProfileInfoShown,
        CommandResult::OpenProfileBrowser,
    ]
}
