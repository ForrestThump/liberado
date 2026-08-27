//! # liberado-heuristics-tuner
//!
//! Automates the manual "run eval → read the misses → tweak a role's system prompt → rerun" loop
//! (`docs/future-work/heuristics-tuning-engine-plan.md`). Started dispatcher-only: generate and score
//! candidate system prompts against `liberado-eval`'s existing scenario set via a
//! beam-search-with-restarts loop, then propose the best candidate as a diff + rubric for a human
//! to review — nothing here ever writes to the real `DEFAULT_SYSTEM_PROMPT`. Extended to the
//! executor and subagent layers (`DIRECT_INSTRUCTIONS`/`SUBAGENT_PREAMBLE` — both run through
//! `liberado_executor::Executor::execute`, so they share the same scoring/search machinery), scored
//! by actually driving a mocked `Executor::execute` tool loop per trial
//! (`tool_loop_scoring`/`tool_scenarios`) rather than a single classification call —
//! plus a **coder** layer that scores Liberado coding-worker prompts against real temp git
//! workspaces via `liberado-coder-agent`. `TunerConfig::layer` selects which role a session tunes.
//! Never writes to the real prompt consts; a human hand-adopts a winning candidate.
//!
//! Dispatcher scoring only calls `Dispatcher::dispatch`. Executor/subagent scoring uses a scripted
//! mock `ToolRuntime` (never a real MCP). Coder scoring mutates only **tempdir** workspaces created
//! for the trial — never the Liberado repo itself.

pub mod candidate;
pub mod coder_curriculum_mock;
pub mod coder_generation;
pub mod coder_scenarios;
pub mod coder_scoring;
pub mod coder_search;
pub mod config;
pub mod draft_proposal;
pub mod generation;
mod generation_engine;
pub mod rubric;
pub mod scoring;
pub mod search;
pub mod tool_loop_generation;
pub mod tool_loop_scoring;
pub mod tool_loop_search;
pub mod tool_scenarios;

pub use candidate::{Candidate, CandidateOrigin};
pub use coder_curriculum_mock::{
    mock_script_for, mockable_scenarios, run_mock_curriculum, score_mock_scenario,
};
pub use coder_scenarios::{
    CoderExpect, CoderScenario, CoderTier, DEFAULT_CODER_SYSTEM_PROMPT, coder_scenarios,
    coder_scenarios_for, greenfield_scenario_names,
};
pub use coder_scoring::{CoderFitness, CoderScoredScenario, CoderTrial, score_coder_candidate};
pub use coder_search::{CoderGenerationRecord, CoderTunerResult, run_coder_tuner};
pub use config::{ConfigError, Layer, TunerConfig};
pub use draft_proposal::{
    CoderDraftProposal, DEFAULT_CODER_PROMPT_PATH, PrFactoryTaskPayload,
    build_coder_draft_proposal, build_pr_factory_task, format_proposal_markdown,
    write_coder_draft_proposal,
};
pub use generation::GenerationError;
pub use scoring::{CandidateFitness, ScenarioTrial, ScoredScenario};
pub use search::{Budget, GenerationRecord, TunerResult, run_tuner};
pub use tool_loop_scoring::{
    ToolLoopFitness, ToolLoopScoredScenario, ToolLoopTrial, score_executor_candidate,
};
pub use tool_loop_search::{
    ExecutorGenerationRecord, ExecutorTunerResult, run_executor_tuner, run_subagent_tuner,
};
pub use tool_scenarios::{ToolLoopExpect, ToolLoopScenario, tool_loop_scenarios};
