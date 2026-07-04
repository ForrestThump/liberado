# liberado-heuristics-tuner — automated prompt tuning

Automates the manual "run eval → read the misses → tweak a role's system prompt → rerun" loop
`liberado-eval` already documents doing by hand: generate and score candidate system prompts via a
beam-search-with-restarts loop, then propose the best candidate as a diff + rubric for a human to
review. **Never writes to a real prompt const** — a human hand-adopts a winning candidate, the same
trust boundary as riggers' draft-PR pattern (Decision 14: agents don't write config/prompts). Full
design and live-run findings: `docs/roadmap/heuristics-tuning-engine-plan.md`.

## Three tunable layers, one config selector

`TunerConfig::layer` (`tuner.toml`'s `layer`/`TUNER_LAYER`, default `Dispatcher`) picks which role a
session tunes:

| Layer | Seeds from | Scored by | Turn budget |
|---|---|---|---|
| `Dispatcher` | `liberado_dispatcher::DEFAULT_SYSTEM_PROMPT` | `Dispatcher::dispatch` — a single classification call, no execution | n/a |
| `Executor` | `liberado_orchestrator::DIRECT_INSTRUCTIONS` | a real (mocked) `Executor::execute` tool loop | `DIRECT_MAX_TURNS` (4) |
| `Subagent` | `liberado_orchestrator::SUBAGENT_PREAMBLE` | same tool-loop machinery as `Executor` | `liberado_executor::DEFAULT_MAX_TURNS` (8) |

The dispatcher path is cheap (no execution needed — deterministic classification vs. a fixed label);
the executor/subagent paths are materially more expensive and slower (a real, if mocked, multi-turn
tool loop per trial) and were added later, deliberately kept *parallel to*, not a generalization of,
the dispatcher path's types — see `docs/roadmap/heuristics-tuning-engine-plan.md`'s executor/subagent
extension for why some duplication (`select_beam`/`select_beam_executor`,
`advance_beam`/`advance_beam_executor`) was an accepted tradeoff while the elitism logic was new and
unproven.

## Modules

- `config.rs` — `TunerConfig`/`Layer`, resolved from `tuner.toml` + env vars (three-layer precedence,
  same pattern as `topology.toml`/`policy.toml`/`tuning.toml`). `OPENROUTER_API_KEY` is the one
  secret, env-only (Decision 10).
- `candidate.rs` — `Candidate`/`CandidateOrigin` (cold-start vs. mutated-from-parent lineage).
- `scoring.rs` — dispatcher-layer scoring: `score_candidate`, `ScoredScenario`/`ScenarioTrial`,
  `CandidateFitness` (asymmetric aggregation — `unsafe_acts` is a worst-case count, never averaged
  away; `accuracy`/`safe_default_rate` are legitimate mean pass rates across (model, sample) trials).
- `tool_scenarios.rs` — executor/subagent-layer scenario data: `ToolLoopScenario`/`ToolLoopExpect`
  (which tools must/must-not be called, what final `Report::outcome` is expected) — hand-written,
  same style as `liberado_eval::scenarios()` but for tool-loop correctness, not a classification label.
- `tool_loop_scoring.rs` — executor/subagent-layer scoring: `ScriptedToolRuntime` (a mock `ToolRuntime`
  giving each scenario's tools their own canned result, unlike test doubles elsewhere that return one
  fixed value for every tool), `score_executor_candidate`, `ToolLoopFitness` (mirrors
  `CandidateFitness`'s asymmetry; `outcome_match_rate` is the `safe_default_rate` analog).
- `generation.rs` — the two meta-LLM calls that produce candidates: `cold_start`/`mutate` (dispatcher)
  and `cold_start_executor`/`mutate_executor` (executor/subagent, reused for both — same underlying
  job description fits either role).
- `search.rs` — the beam-search loop and its shared `Budget` (a plain LLM-call countdown for the
  whole session — distinct from `liberado_executor::Budget`, which is turns-per-task; same name,
  different crates, don't conflate). `run_tuner` (dispatcher); `run_executor_tuner`/
  `run_subagent_tuner` are both thin wrappers over a private `run_tool_loop_tuner(config, seed_prompt,
  max_turns)`. `select_beam`/`advance_beam` (dispatcher) and `select_beam_executor`/
  `advance_beam_executor` (executor+subagent, shared) implement **elitism**: the incumbent beam is
  included in the same selection as each generation's new pool, so a generation can never regress the
  beam below its best-so-far — a real bug (an independent cold start could permanently evict a much
  better incumbent it was never compared against) was found and fixed here via a live comprehensive
  run that regressed accuracy 0.77→0.33.
- `rubric.rs` — `format_rubric`/`format_executor_rubric`: the human-facing proposal artifact (metric
  deltas, named scenario regressions/fixes, per-model consistency, a full per-scenario diagnostic
  breakdown, the tuning model's own justification for why the change should generalize).
- `main.rs` — entry point: resolve config, run the layer-appropriate session, save every generation's
  best candidate (not just the final winner) under `<LIBERADO_DATA_DIR>/tuner/<run-timestamp>/`.

## Real findings from live use, not just design

Live tuning found and fixed two real bugs beyond prompt wording itself:

1. **A beam-search elitism bug** (`search.rs`) — fixed; see above.
2. **An engine-level bug in `liberado-executor`** — `REPORT_NUDGE` was biasing the model away from
   continuing a multi-step plan. Fixed in `liberado-executor` directly (benefits every consumer of
   `Executor::execute`, not just this tuner). The underlying reliability gap this partially fixed —
   multi-step tool chaining still isn't fully reliable — is a project-level open finding, not just a
   tuner curiosity: `docs/roadmap/multi-step-execution-reliability-finding.md`.

## Dependencies

- Depends on: `liberado-common`, `liberado-dispatcher`, `liberado-eval` (dispatcher-layer scenarios),
  `liberado-executor`/`liberado-orchestrator` (executor/subagent-layer scenarios and seed prompts),
  `liberado-provider`/`liberado-provider-openrouter` (the concurrent, many-models-behind-one-key
  backend that makes scoring a whole generation's candidate pool concurrently affordable).
- Depended on by: nobody — it's a standalone dev tool (binary), not a build dependency of the running
  system, same posture as `liberado-eval`.

## Tests

77+ unit tests across the modules above (aggregation asymmetry, elitism, config resolution, scenario
sanity checks, scripted mock-runtime scoring). A handful of `#[ignore]`d live tests
(`live_end_to_end`, `live_end_to_end_executor`, `live_end_to_end_subagent`) require
`OPENROUTER_API_KEY` and real network access — run explicitly, not part of `cargo test --workspace`.
