//! `liberado-eval`'s library half: the labeled scenario data (`scenarios`) and the reusable
//! scoring logic ([`scoring`]) the `liberado-eval` binary runs against a real dispatcher. Exposed
//! as a library — not just a binary — so a future consumer (the planned heuristics tuning engine,
//! `docs/future-work/heuristics-tuning-engine-plan.md`) can score dynamically-generated scenarios
//! against the same rules instead of re-deriving them.

pub mod scenarios;
pub mod scoring;

pub use scenarios::{ExpectKind, Scenario, scenarios};
pub use scoring::{ScenarioOutcome, score};
