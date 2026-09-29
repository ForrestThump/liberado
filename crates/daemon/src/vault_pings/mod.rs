//! Vault notes that mechanical reminders read. No model, no second task store.
//!
//! Tasks are Obsidian Tasks lines in any note the walker is allowed to open.
//! Events are [Full Calendar](https://github.com/obsidian-community/obsidian-full-calendar)
//! frontmatter, plus a titled note under a `calendar/` folder (the vault layout in
//! `docs/spec/liberado-architecture.md`). Dates are calendar dates in the operator zone.
//! `cron_expr` stays UTC.

mod date;
mod events;
mod remind;
mod tasks;
mod tick;
mod walk;

pub(crate) use events::event_ping_message;
pub(crate) use tasks::task_ping_message;
pub(crate) use tick::{ReminderTick, reminder_message};

#[cfg(test)]
pub(crate) use events::{EVENT_HORIZON_DAYS, VaultEvent, event_in_window};
#[cfg(test)]
pub(crate) use tasks::parse_task_line;

#[cfg(test)]
#[path = "../vault_pings_tests.rs"]
mod vault_pings_tests;
