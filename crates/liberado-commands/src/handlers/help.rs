use crate::context::CommandContext;
use crate::result::CommandResult;

const HELP_TEXT: &str = "\
Slash commands:

  /quit       quit the client
  /exit       quit the client (alias)
  /new        start a new conversation
  /clear      clear the chat display (local only)
  /help       show this help
  /status     show daemon connection info
  /session    session control (info, list, switch, close)
  /theme      theme switching (list, set, reload)
  /model      show model info
  /fork       fork current conversation (server support pending)";

pub fn handle(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    ctx.push_system_message(HELP_TEXT.into());
    vec![CommandResult::HelpShown]
}
