# liberado-heuristics-tuner — automated prompt tuning

Automates the manual "run eval → read the misses → tweak a role's system prompt → rerun" loop
`liberado-eval` already documents doing by hand: generate and score candidate system prompts via a
beam-search-with-restarts loop, then propose the best candidate as a diff + rubric for a human to
review. **Never writes to a real prompt const** — a human hand-adopts a winning candidate, the same
trust boundary as riggers' draft-PR pattern (Decision 14: agents don't write config/prompts). Full
design and live-run findings: `docs/future-work/heuristics-tuning-engine-plan.md`.

## Three tunable layers, one config selector

`TunerConfig::layer` (`tuner.toml`'s `layer`/`TUNER_LAYER`, default `Dispatcher`) picks which role a
session tunes:

| Layer | Seeds from | Scored by | Turn budget |
|---|---|---|---|
| `Dispatcher` | `liberado_dispatcher::DEFAULT_SYSTEM_PROMPT` | `Dispatcher::dispatch` — a single classification call, no execution | n/a |
| `Executor` | `liberado_orchestrator::DIRECT_INSTRUCTIONS` | a real (mocked) `Executor::execute` tool loop | `DIRECT_MAX_TURNS` (4) |
| `Subagent` | `liberado_orchestrator::SUBAGENT_PREAMBLE` | same tool-loop machinery as `Executor` | `liberado_executor::DEFAULT_MAX_TURNS` (8) |
| `Coder` | `DEFAULT_CODER_SYSTEM_PROMPT` / `prompts/coder/coder.md` | real temp git repo + `liberado-coder-agent` + coding tools | 12 turns |

The dispatcher path is cheap (no execution needed — deterministic classification vs. a fixed label);
the executor/subagent paths are materially more expensive and slower (a real, if mocked, multi-turn
tool loop per trial) and were added later, deliberately kept *parallel to*, not a generalization of,
the dispatcher path's types — see `docs/future-work/heuristics-tuning-engine-plan.md`'s executor/subagent
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
- `tool_scenarios.rs` / `tool_loop_scoring.rs` / `tool_loop_search.rs` — executor/subagent tool-loop
  scenarios, mock runtime scoring, beam search.
- **`coder_curriculum_mock.rs`** — CI mock scripts for smoke/core curriculum (no API key)
- **`draft_proposal.rs`** — meta-loop export: eval deltas → `PROPOSAL.md` / `proposal.json` /
  `proposed/` / `pr_factory_task.json` (Decision 14; never auto-applies prompts)
- **`coder_scenarios.rs` / `coder_scoring.rs` / `coder_generation.rs` / `coder_search.rs`** — coder
  layer: real temp git workspaces + `liberado-coder-agent`, diff/path expectations, meta mutate,
  `run_coder_tuner`. Metrics: coding accuracy, nonempty-diff rate, unsafe path touches.
- `generation.rs` / `search.rs` — dispatcher meta LLM + beam search; shared `Budget`.
- `rubric.rs` — `format_rubric` / `format_executor_rubric` / **`format_coder_rubric`**.
- `main.rs` — `layer` selects dispatcher | executor | subagent | **coder**; saves under
  `<LIBERADO_DATA_DIR>/tuner/<run-timestamp>/`.

## Real findings from live use, not just design

Live tuning found and fixed two real bugs beyond prompt wording itself:

1. **A beam-search elitism bug** (`search.rs`) — fixed; see above.
2. **An engine-level bug in `liberado-executor`** — `REPORT_NUDGE` was biasing the model away from
   continuing a multi-step plan. Fixed in `liberado-executor` directly (benefits every consumer of
   `Executor::execute`, not just this tuner). The underlying reliability gap this partially fixed —
   multi-step tool chaining still isn't fully reliable — is a project-level open finding, not just a
   tuner curiosity: `docs/future-work/archive/multi-step-execution-reliability-finding.md`.

## Dependencies

- Depends on: `liberado-common`, `liberado-dispatcher`, `liberado-eval` (dispatcher-layer scenarios),
  `liberado-executor`/`liberado-orchestrator` (executor/subagent-layer scenarios and seed prompts),
  `liberado-coder-agent`/`liberado-coder-core` (coder-layer workspace scoring),
  `liberado-provider`/`liberado-provider-openai-compat` (OpenRouter-backed scoring/meta providers).
- Depended on by: nobody — it's a standalone dev tool (binary), not a build dependency of the running
  system, same posture as `liberado-eval`.

## Tests

90+ unit tests across the modules above (aggregation asymmetry, elitism, config resolution, scenario
sanity checks, scripted mock-runtime scoring, coder mock workspace scoring). A handful of
`#[ignore]`d live tests (`live_end_to_end`, `live_end_to_end_executor`, `live_end_to_end_subagent`)
require `OPENROUTER_API_KEY` and real network access — run explicitly, not part of
`cargo test --workspace`.

## Running the coder layer

```bash
# smoke: few scenarios, small budget
export OPENROUTER_API_KEY=...
export TUNER_LAYER=coder
export TUNER_MAX_SCENARIOS=2
export TUNER_CALL_BUDGET=80
export TUNER_MAX_GENERATIONS=1
cargo run -p liberado-heuristics-tuner
# proposals under $LIBERADO_DATA_DIR/tuner/<timestamp>/final.txt
```
