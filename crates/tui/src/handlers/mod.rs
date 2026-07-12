//! Keyboard and mouse input handlers for the Liberado TUI.
//!
//! Each handler is a free function `pub(crate) fn handle(app: &mut App, key) → Vec<Effect>`.
//! `App::handle_key()` and `App::handle_mouse()` dispatch to these modules based on focus.

pub mod chat;
pub mod input;
pub mod models;
pub mod mouse;
pub mod sidebar;
