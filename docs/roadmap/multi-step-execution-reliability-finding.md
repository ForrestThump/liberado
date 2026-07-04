# Multi-step tool-chaining reliability — an open, project-level finding

**Status**: Open. One real contributing bug found and fixed (below); the gap persists, partially
narrowed, and is not fully understood. Surfaced 2026-07-06 by `liberado-heuristics-tuner`'s
executor/subagent-layer live tuning (`docs/roadmap/heuristics-tuning-engine-plan.md`'s "Executor-layer
live smoke tests" and "Subagent-layer" sections carry the raw run-by-run data this doc summarizes and
elevates). Not chased further this session per explicit user direction — this doc exists so it isn't
lost, and so the next person picking it up doesn't have to re-derive the evidence from tuner run logs.

## Why this is a project-level problem, not a tuner curiosity

Liberado's whole dispatch architecture rests on a division of labor: `ExecuteDirect` handles simple,
few-step goals; `DispatchSubagent` hands complex, multi-step goals to a narrowly-scoped subagent with
more room to work (`docs/architecture/overview.md`'s three pillars, `crates/dispatcher/ARCHITECTURE.md`).
Both routes terminate in the *same* engine — `liberado_executor::Executor::execute` — just with
different seed prompts (`DIRECT_INSTRUCTIONS`/`SUBAGENT_PREAMBLE`, `crates/orchestrator/src/lib.rs`)
and turn budgets (4 vs. 8). If that shared engine cannot reliably chain two sequential, clearly-stated
tool calls into a coherent completed report, the *entire premise* of `DispatchSubagent` — "hand it
something multi-step and it'll handle the complexity" — is undermined for any real goal shaped that
way: "look this up, then write a note about it," "read X and Y, then combine them," "check the status,
then act on it." That's an extremely common goal shape, not an edge case. A dispatcher that routes
correctly is worthless if the layer it routes *to* silently fails to finish the job — and post-mortem
this project's central architectural bet is that safety and correctness live in the deterministic
layers around the model, not in prompt wording alone; this finding is a live test of exactly how far
that bet extends once the model itself is asked to sequence multiple actions.

## The scenario that surfaced it

`crates/heuristics-tuner/src/tool_scenarios.rs`'s `multi-step-research` scenario:

- **Goal**: "Research how the turbomcp transport layer works and save a summary note in the vault."
- **Tools available**: `deepwiki` (research), `vault` (write) — both genuinely required.
- **Expectation**: both tools called, final `Report::outcome == Succeeded`.

This is about as simple a two-step chain as a real goal can have — no ambiguity about which tools to
use, no missing information, both tools described plainly. `liberado-heuristics-tuner`'s
`ScriptedToolRuntime` gives each tool a canned, coherent result, so nothing about the mock is what's
failing (confirmed directly — see "Ruled out" below).

## Evidence timeline

All runs against `deepseek/deepseek-v4-flash` via OpenRouter, real live calls, no mocking of the model.

| Run | Layer | Turn budget | Prompt | `calls_matched` | `outcome_matched` |
|---|---|---|---|---|---|
| 1 | Executor | 4 | baseline `DIRECT_INSTRUCTIONS` | 0/1 | — |
| 2 | Executor | 4 | winning mutation (added rules, no explicit multi-step instruction) | 0/3 | 0/3 |
| 3 | Executor | 4 | winning mutation, *explicitly* said "you MUST call each required tool exactly once... e.g. research then save" | 0/3 | 0/3 |
| 4 | Executor | 4 | different winning mutation, also explicit about sequencing | 0/3 | 0/3 |
| — | **Fixed `REPORT_NUDGE` (see below), same day** | | | | |
| 5 | Executor | 4 | winning mutation, post-fix | **1/3** | 0/3 |
| 6 | Subagent | 8 | baseline `SUBAGENT_PREAMBLE`, post-fix | 0/1 | 0/1 |

Runs 2-4 total **0/9 `calls_matched`** across two independent tuning sessions with different winning
prompts, **both of which explicitly instructed the exact behavior needed** — a 100% failure rate under
an on-the-nose instruction doesn't fit "the model sometimes ignores wording." That was the signal that
something structural, not prompt-level, was at fault.

## Root cause found and fixed: `REPORT_NUDGE`

`Executor::run_loop` (`crates/executor/src/lib.rs`) injects a fixed message the first time a model
replies in prose instead of calling a tool — meant as a backstop against the loop ending in
unstructured text. Before the fix, that message read:

> *"Before finishing, call the `submit_report` tool with your final result. Do not reply in plain
> text."*

This unconditionally pushes toward wrapping up — it never offers "keep going." A model that pauses to
narrate in prose after the first tool call (ordinary behavior, especially for a smaller model like
DeepSeek) gets pressured to file immediately rather than continue to the second required call. Since
`REPORT_NUDGE` is a separate constant in `liberado-executor`, injected *after* the system prompt is
already in play, **no amount of `DIRECT_INSTRUCTIONS`/`SUBAGENT_PREAMBLE` wording could ever
out-compete it** — this is the mechanism that explains runs 2-4's 100% failure rate under explicit
instructions.

**Fixed** (`crates/executor/src/lib.rs`, `REPORT_NUDGE`): reworded to explicitly offer both options —
continue acting if the goal isn't finished, or file if it genuinely is (or the model is stuck) — rather
than unconditionally pushing to finish. This is an engine-level fix, independent of whatever
`DIRECT_INSTRUCTIONS`/`SUBAGENT_PREAMBLE` text the tuner eventually settles on, and benefits every
consumer of `Executor::execute`, not just the tuner (production `ExecuteDirect`/`DispatchSubagent`
included).

## Ruled out

- **Turn budget starvation**: the sequence needs 3 turns (call `deepwiki`, call `vault`,
  `submit_report`), comfortably inside even the executor's tighter 4-turn cap. Run 6's *looser* 8-turn
  subagent budget didn't fix it either (single sample, not strong evidence alone, but consistent).
- **A bug in the tuner's own scoring logic**: two regression tests
  (`score_one_matches_a_well_behaved_multi_tool_trial`,
  `score_one_matches_regardless_of_required_call_order` in `tool_loop_scoring.rs`) confirm a trial
  that genuinely calls both required tools, in either order, is correctly classified as matched —
  ruled out before attributing the failure to model/engine behavior.
- **Silent false-success auto-wrap**: a *second* consecutive prose reply auto-wraps as a `Report` with
  `Outcome::Succeeded` hardcoded (`prose_report()`). If that path were firing, `outcome_matched` would
  be `true` despite `calls_matched` being `false`. It wasn't — `outcome_matched` was *also* 0 in every
  pre-fix trial, meaning the model was **honestly self-reporting failure**, not being silently
  papered over as a false success.

## What's still open (not chased further this session, per explicit direction)

1. **`multi-step-research` still doesn't land on a clean `Succeeded`** even post-fix, even in the one
   trial (run 5) that *did* call both required tools. `calls_matched` improved 0/9 → 1/3, but
   `outcome_matched` stayed 0/3. Something beyond the nudge — possibly the model hedging toward
   `PartiallySucceeded` on compound tasks, or genuine uncertainty about whether a canned tool result
   "really" constitutes a completed write — is still preventing a clean success report.
2. **Whether this is DeepSeek-specific or general.** Every run so far used one model
   (`deepseek/deepseek-v4-flash`) for cost reasons. Whether other models (Gemini, Grok, GPT-5-nano —
   already in the dispatcher-layer scoring set) show the same failure shape is unknown; if they don't,
   this narrows to a DeepSeek weakness rather than an engine-universal one — genuinely important for
   deciding how much to invest here vs. just avoiding DeepSeek for delegated multi-step work.
3. **`honest-failure-report` regressed** in the same post-fix run (was passing, dropped to 1/3) —
   plausibly real-model sampling noise at only 3 samples (this project already has a separately
   documented finding that DeepSeek's API isn't perfectly deterministic run-to-run,
   `heuristics-tuning-engine-plan.md`'s "Real-model verification" section), but unconfirmed either way.
4. **No way to see *why*, currently — only outcomes.** Diagnosing (1) further today means guessing
   from aggregate pass/fail booleans, the same way this session found the `REPORT_NUDGE` bug (reading
   engine source and reasoning about turn-by-turn behavior speculatively, then confirming after the
   fact with a live run). `docs/ideas/model-reasoning-introspection-idea.md` captures a better
   diagnostic approach for next time: reading an actually-exposed reasoning trace where a model
   provides one, not a post-hoc "explain your choice" interjection (which is a soft, unreliable signal
   for non-reasoning models — a real limitation of that idea, corrected in the doc itself).

## How to reproduce / pick this back up

```
# config/tuner.toml (gitignored — see config.example/tuner.toml for the template)
layer = "executor"            # or "subagent"
scoring_models = ["deepseek/deepseek-v4-flash"]
samples_per_scenario = 5      # or higher, for firmer statistical signal than this session's 1-3
max_scenarios = 2             # limits to single-lookup + multi-step-research if declared first
beam_width = 1
cold_starts_per_generation = 1
mutations_per_candidate = 1
max_generations = 1
call_budget = 40
```

Then `cargo run -p liberado-heuristics-tuner` with `OPENROUTER_API_KEY` set. The rubric's
"Full scenario breakdown" section (`format_executor_rubric`, added this session specifically for this
kind of diagnosis) reports `calls_matched`/`unsafe calls`/`outcome_matched` per scenario unconditionally,
not just on a diff or when samples disagree — read that directly rather than re-deriving it.

## Cross-references

- Full run-by-run data and narrative: `docs/roadmap/heuristics-tuning-engine-plan.md`'s
  "Executor-layer live smoke tests" and "Subagent-layer" sections.
- The fix itself: `crates/executor/src/lib.rs`'s `REPORT_NUDGE`.
- The scenario definition: `crates/heuristics-tuner/src/tool_scenarios.rs`.
- The diagnostic idea for next time: `docs/ideas/model-reasoning-introspection-idea.md`.
- Engine architecture: `crates/executor/ARCHITECTURE.md`.
