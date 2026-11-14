//! Vault notes that mechanical reminders read. No model, no second task store.
//!
//! Tasks are Obsidian Tasks lines in any note the walker is allowed to open.
//! Events are [Full Calendar](https://github.com/obsidian-community/obsidian-full-calendar)
//! frontmatter, plus a titled note under a `calendar/` folder (the vault layout in
//! `docs/spec/liberado-architecture.md`). Dates are calendar dates in the operator zone.
//! `cron_expr` stays UTC.

mod completions;
mod date;
mod events;
mod local_time;
mod remind;
mod tasks;
mod tick;
mod walk;

pub(crate) use completions::record_task_completions;
pub(crate) use events::event_ping_message;
pub(crate) use tasks::task_ping_message;
pub(crate) use tick::{DueReminderRow, ReminderTick, due_reminder_rows, reminder_message};

/// One completion pass, the same pass `vault-reminder-tick` runs.
///
/// `now` is local civil time in `zone`. The fire-key file is not used.
pub fn record_vault_task_completions(
    root: &std::path::Path,
    now: chrono::NaiveDateTime,
    zone: &str,
    settings: &liberado_config::VaultRemindersConfig,
    outbox: &liberado_outbox::Outbox,
) -> Result<(), liberado_outbox::Error> {
    let tick = ReminderTick {
        now,
        settings: settings.clone(),
        state_path: root.join("vault-reminder-fires.json"),
        zone: zone.to_string(),
    };
    record_task_completions(root, &tick, outbox)
}

#[cfg(test)]
pub(crate) use events::{EVENT_HORIZON_DAYS, VaultEvent, event_in_window};

#[cfg(test)]
#[path = "../vault_pings_event_tests.rs"]
mod vault_pings_event_tests;
#[cfg(test)]
#[path = "../vault_pings_task_tests.rs"]
mod vault_pings_task_tests;
