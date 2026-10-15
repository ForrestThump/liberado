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

#[path = "turn_failure_tests.rs"]
mod turn_failure_tests;

#[path = "grants.rs"]
mod grants;

#[path = "grant_gate_tests.rs"]
mod grant_gate_tests;

#[path = "dispatch.rs"]
mod dispatch;

#[path = "prompt.rs"]
mod prompt;

#[path = "compaction.rs"]
mod compaction;

#[path = "compaction_models.rs"]
mod compaction_models;

#[path = "pending_model_tests.rs"]
mod pending_model_tests;

#[path = "compaction_history.rs"]
mod compaction_history;

#[path = "model.rs"]
mod model;

#[path = "provider_route_tests.rs"]
mod provider_route_tests;

#[path = "metering.rs"]
mod metering;

#[path = "turn_clock_integration_tests.rs"]
mod turn_clock_integration_tests;
