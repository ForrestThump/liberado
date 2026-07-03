//! # liberado-heuristics-tuner
//!
//! Automates the manual "run eval → read the misses → tweak the dispatcher's system prompt →
//! rerun" loop that `liberado-eval` already documents doing by hand
//! (`docs/roadmap/heuristics-tuning-engine-plan.md`). v1 scope is the dispatcher layer only:
//! generate and score candidate system prompts against `liberado-eval`'s existing scenario set via
//! a beam-search-with-restarts loop, then propose the best candidate as a diff + rubric for a
//! human to review — nothing here ever writes to the real `DEFAULT_SYSTEM_PROMPT`.
//!
//! No real tool call or vault write happens anywhere in this crate: scoring only calls
//! `Dispatcher::dispatch`, which is a pure classification decision.

pub mod candidate;
pub mod config;
pub mod generation;
pub mod rubric;
pub mod scoring;
pub mod search;

pub use candidate::{Candidate, CandidateOrigin};
pub use config::{ConfigError, TunerConfig};
pub use generation::GenerationError;
pub use scoring::{CandidateFitness, ScenarioTrial, ScoredScenario};
pub use search::{Budget, GenerationRecord, TunerResult, run_tuner};
