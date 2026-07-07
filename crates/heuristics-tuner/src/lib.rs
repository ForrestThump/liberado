//! # liberado-heuristics-tuner
//!
//! Automates the manual "run eval → read the misses → tweak a role's system prompt → rerun" loop
//! (`docs/roadmap/heuristics-tuning-engine-plan.md`). Started dispatcher-only: generate and score
//! candidate system prompts against `liberado-eval`'s existing scenario set via a
//! beam-search-with-restarts loop, then propose the best candidate as a diff + rubric for a human
//! to review — nothing here ever writes to the real `DEFAULT_SYSTEM_PROMPT`. Extended to the
//! executor and subagent layers (`DIRECT_INSTRUCTIONS`/`SUBAGENT_PREAMBLE` — both run through
//! `liberado_executor::Executor::execute`, so they share the same scoring/search machinery), scored
//! by actually driving a mocked `Executor::execute` tool loop per trial
//! (`tool_loop_scoring`/`tool_scenarios`) rather than a single classification call —
//! `TunerConfig::layer` selects which role a session tunes. Never writes to the real prompt consts
//! in any case; a human hand-adopts a winning candidate.
//!
//! No real tool call or vault write happens anywhere in this crate: dispatcher-layer scoring only
//! calls `Dispatcher::dispatch` (a pure classification decision), and executor/subagent-layer
//! scoring only ever runs against a scripted mock `ToolRuntime`, never a real MCP.

pub mod candidate;
pub mod config;
pub mod generation;
pub mod rubric;
pub mod scoring;
pub mod search;
pub mod tool_loop_generation;
pub mod tool_loop_scoring;
pub mod tool_loop_search;
pub mod tool_scenarios;

pub use candidate::{Candidate, CandidateOrigin};
pub use config::{ConfigError, Layer, TunerConfig};
pub use generation::GenerationError;
pub use scoring::{CandidateFitness, ScenarioTrial, ScoredScenario};
pub use search::{Budget, GenerationRecord, TunerResult, run_tuner};
pub use tool_loop_scoring::{ToolLoopFitness, ToolLoopScoredScenario, ToolLoopTrial, score_executor_candidate};
pub use tool_loop_search::{
    ExecutorGenerationRecord, ExecutorTunerResult, run_executor_tuner, run_subagent_tuner,
};
pub use tool_scenarios::{ToolLoopExpect, ToolLoopScenario, tool_loop_scenarios};
