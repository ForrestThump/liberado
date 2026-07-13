//! Session-focus commands (`/sessions`, `/join <id>`, `/back`) for the unified-`Session` model.
//!
//! These are thin: the handler validates input and emits a [`CommandResult`] the surface maps to
//! its own focus machinery (open the switcher, subscribe to a goal-session stream, or return to the
//! primary chat). No shared state changes here — focus is a per-surface concern.

use crate::context::CommandContext;
use crate::result::CommandResult;

/// `/sessions` — open the unified session switcher (primary chat + goal sessions in one list).
pub fn open_switcher(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    vec![CommandResult::OpenGoalSwitcher]
}

/// `/join <id>` — move input focus onto a goal session by id (or id prefix). Empty id prints usage.
pub fn join(id: &str, ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    let id = id.trim();
    if id.is_empty() {
        ctx.push_system_message(
            "Usage: /join <session-id>\nOr run /sessions to pick one from the switcher.".into(),
        );
        return vec![CommandResult::None];
    }
    vec![CommandResult::JoinGoalSession { id: id.to_string() }]
}

/// `/back` — return input focus to the primary chat, leaving any joined goal session.
pub fn back(ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    vec![CommandResult::BackToPrimary]
}

/// `/spawn <profile|domain> <goal>` — start a new interactive goal session and focus it. The first
/// argument may name a domain pack (`life`, `coding`) or a `[[session_profiles]]` hat (`research`);
/// the server resolves profile-first. Empty argument prints usage.
pub fn spawn(domain: &str, goal: &str, ctx: &mut dyn CommandContext) -> Vec<CommandResult> {
    ctx.clear_input();
    let domain = domain.trim();
    let goal = goal.trim();
    if domain.is_empty() || goal.is_empty() {
        ctx.push_system_message(
            "Usage: /spawn <profile|domain> <goal>\n\
             e.g. /spawn life \"plan my week\"   (the life pack)\n\
             e.g. /spawn research \"compare X\"  (a narrower hat on the same pack)"
                .into(),
        );
        return vec![CommandResult::None];
    }
    vec![CommandResult::SpawnGoalSession {
        domain: domain.to_string(),
        goal: goal.to_string(),
    }]
}
