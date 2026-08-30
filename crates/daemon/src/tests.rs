//! `Daemon` integration tests grouped by subsystem.
//! Shared builders live in `test_fixtures`.

#[path = "tests/test_fixtures.rs"]
pub(crate) mod test_fixtures;

#[path = "tests/helpers.rs"]
mod helpers;

#[path = "tests/watcher.rs"]
mod watcher;

#[path = "tests/reactions.rs"]
mod reactions;

#[path = "tests/pools.rs"]
mod pools;

#[path = "tests/proposals.rs"]
mod proposals;

#[path = "tests/sessions.rs"]
mod sessions;

#[path = "tests/guards.rs"]
mod guards;

#[path = "tests/lifecycle.rs"]
mod lifecycle;
