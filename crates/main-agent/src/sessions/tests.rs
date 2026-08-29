//! `ChatSessions` tests grouped by public state transition.
//! Shared builders live in `test_fixtures`.

#[path = "test_fixtures.rs"]
mod test_fixtures;

#[path = "lifecycle.rs"]
mod lifecycle;

#[path = "titles.rs"]
mod titles;

#[path = "durability.rs"]
mod durability;

#[path = "grants.rs"]
mod grants;

#[path = "dispatch.rs"]
mod dispatch;

#[path = "prompt.rs"]
mod prompt;

#[path = "compaction.rs"]
mod compaction;

#[path = "compaction_models.rs"]
mod compaction_models;

#[path = "compaction_history.rs"]
mod compaction_history;

#[path = "model.rs"]
mod model;

#[path = "metering.rs"]
mod metering;
