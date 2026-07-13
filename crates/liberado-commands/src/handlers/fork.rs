use crate::context::CommandContext;
use crate::result::CommandResult;

/// `/fork` — branch this conversation, keeping the original.
///
/// `/fork` copies the whole thing as it stands; `/fork <n>` copies only through your Nth turn and
/// its reply, so you land back at the moment just before you typed turn N+1 and can take a different
/// path. Either way the original is untouched and still in the switcher — that is what a fork *is*,
/// and the reason the store copies rather than points (see `SessionStore::fork_session`).
pub fn handle(ctx: &mut dyn CommandContext, after_turn: Option<u32>) -> Vec<CommandResult> {
    ctx.clear_input();
    let Some(id) = ctx.active_session_id().map(String::from) else {
        ctx.push_system_message(
            "No conversation to fork. Say something first, or open one with /sessions.".into(),
        );
        return vec![CommandResult::None];
    };

    match after_turn {
        // Turn 0 would mean "keep none of it", which is just a new conversation — say so rather
        // than forking something the human didn't ask for.
        Some(0) => {
            ctx.push_system_message(
                "Turns are numbered from 1. Use /fork 1 to keep your first turn, or a bare /fork \
                 to branch the whole conversation."
                    .into(),
            );
            vec![CommandResult::None]
        }
        Some(n) => {
            ctx.push_system_message(format!(
                "Forking after your turn {n} — the original stays put."
            ));
            vec![CommandResult::ForkRequested {
                parent_id: id,
                after_turn: Some(n),
            }]
        }
        None => {
            ctx.push_system_message("Forking this conversation — the original stays put.".into());
            vec![CommandResult::ForkRequested {
                parent_id: id,
                after_turn: None,
            }]
        }
    }
}
