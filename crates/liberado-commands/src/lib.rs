//! Shared slash-command parser and handlers for Liberado chat clients.
//!
//! Every Liberado UI (TUI, WebUI, CLI) depends on this crate for slash-command
//! parsing (`/help`, `/new`, `/clear`, `/status`, `/theme`, `/model`, `/session`,
//! `/fork`, `/quit`) and the handler logic that executes them.
//!
//! UIs implement the [`CommandContext`] trait to provide state access; the handlers
//! return [`CommandResult`] instructions that each UI translates into its own
//! effect/action system.
//!
//! # Architecture
//!
//! ```text
//! User input ("/theme set dark")
//!   │
//!   ├──► parse("/theme set dark") → SlashCommand::Theme(ThemeCmd::Set("dark"))
//!   │
//!   ├──► dispatch(&cmd, &mut ctx) → Vec<CommandResult>
//!   │         │
//!   │         └──► handler calls ctx.set_theme("dark"), ctx.push_system_message(...)
//!   │
//!   └──► UI maps CommandResult → native effects (HTTP, quit, signal updates)
//! ```
//!
//! # Adding a new slash command
//!
//! 1. Add a variant to [`SlashCommand`].
//! 2. Add a case in [`parse()`].
//! 3. Add a handler function in `handlers/`.
//! 4. Add the route in [`dispatch()`].
//! 5. Add a variant to [`CommandResult`] if the command produces UI-side effects.

pub mod catalog;
pub mod commands;
pub mod constants;
pub mod context;
pub mod dispatch;
pub mod format;
pub mod handlers;
pub mod result;

pub use catalog::{
    COMMAND_CATALOG, CommandSpec, accept_completion, complete_commands, filter_commands,
    ghost_suffix, is_slash_prefix, telegram_commands,
};
pub use commands::{CodingGoalMode, GoalCmd, SessionCmd, SlashCommand, ThemeCmd};
pub use constants::CTX_PCT_DISPLAY_CAP;
pub use context::{CommandContext, StatusInfo};
pub use dispatch::{dispatch, parse};
pub use format::format_uptime;
pub use result::CommandResult;

#[cfg(test)]
mod tests;
