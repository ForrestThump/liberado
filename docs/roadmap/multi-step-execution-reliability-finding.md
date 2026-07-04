# Multi-step tool-chaining reliability — a project-level finding, substantially resolved

**Status**: Substantially resolved (2026-07-04 follow-up session, below) — live-verified 0/6 → 5/6 on
the scenario that originally surfaced this. Originally surfaced 2026-07-06 [sic — see follow-up
session note] by `liberado-heuristics-tuner`'s executor/subagent-layer live tuning
(`docs/roadmap/heuristics-tuning-engine-plan.md`'s "Executor-layer live smoke tests" and
"Subagent-layer" sections carry the raw run-by-run data the first half of this doc summarizes). The
original session found and fixed one real bug (`REPORT_NUDGE`) but left the core gap open and not
fully understood; a follow-up session (same doc, "Follow-up" section below) found the actual root
cause and closed most of it with an engine-level guard, live-verified against real models rather than
just unit tests. The one remaining open item is a fast-finish timing gap, not a doom loop — see
"What's still open" at the end.

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

## Follow-up session (2026-07-04): actual root cause found and fixed

Picked back up at the user's request ("let's start debugging it"). Built a standalone live
reproduction, independent of the tuner's aggregate scoring, specifically to get turn-by-turn
visibility that pass/fail booleans couldn't give (closing open item 4 below, partially — see the
result at the end of this section):
`crates/heuristics-tuner/examples/debug_multistep.rs`, run with `OPENROUTER_API_KEY` set via
`cargo run -p liberado-heuristics-tuner --example debug_multistep`. It runs the real
`multi-step-research` scenario through the actual production `Executor`/`DIRECT_INSTRUCTIONS` against
real models, printing every tool call (name + arguments) and the final `Report` verbatim.

**Root cause**: DeepSeek and Gemini both got stuck calling `deepwiki` 3-6 times in a row, never
reaching the required `vault` call, exhausting the turn budget. Comparison against reference agentic
harnesses (asked directly via the deepwiki MCP: `sst/opencode`, `Kilo-Org/kilocode`, `vinhnx/vtcode`)
confirmed this is a known, named failure mode ("doom loop") that all three defend against with an
**engine-level repeated/near-duplicate-call detector**, not prompt wording — validated further by
`docs/ideas/doomloop_research.md` (independent research the user had another LLM do), which found the
same conclusion in the broader literature (MAST study, Anthropic's own multi-agent post-mortems):
doom loops are a control-flow problem, and "a guard that lives inside the agent's prompt is a
suggestion; a circuit breaker in the control flow is a law."

A first cut checked byte-for-byte argument equality and did **not** fire against the real failure —
DeepSeek was rephrasing the same question every call (`"turbomcp transport layer"` →
`"turbo-mcp transport layer implementation Provider trait stdio HTTP"` → ...), defeating exact
matching entirely. Fixed with a small, local, dependency-free TF-IDF cosine-similarity check between
consecutive same-tool calls' arguments (`args_similarity` in `crates/executor/src/lib.rs`) —
deterministic, no network/embedding-model call, IDF computed just from the pair being compared so a
shared topic word doesn't inflate similarity but a genuine rewording still scores high.
`ARG_SIMILARITY_THRESHOLD = 0.2` was hand-calibrated against real transcript data (rephrasings scored
~0.26/~0.41 pairwise; three genuinely distinct queries scored ~0.10) — a starting point, not a
statistically validated threshold.

**The engine now does, in `Executor::run_loop`**:

1. **Detect** near-duplicate same-tool repeats (`is_doom_loop`, `DOOM_LOOP_THRESHOLD = 3`) and short
   tool-name cycles like A,B,A,B (`detect_short_cycle`, period 2-3) — both checked across the whole
   run, not just one turn (a model can also batch several identical calls into one turn's response;
   covered too).
2. **Escalate on repeat detection**, shared strike counter across both mechanisms:
   - 1st: inject a corrective nudge (`DOOM_LOOP_NUDGE`/`CYCLE_NUDGE`), same nudge-once shape as
     `REPORT_NUDGE`.
   - 2nd (nudge didn't change anything — confirmed live: DeepSeek gave zero visible reasoning
     alongside its repeat calls, so there's no way to tell if it even "saw" the nudge): actually
     **remove the offending tool(s)** from what the model can call for the rest of the task, and tell
     it why (`tool_removed_nudge`/`tools_removed_nudge`) — matches VTCode's `LoopDetector` doing the
     same. Also grants a **one-time, bounded** turn-budget top-up
     (`DOOM_LOOP_RECOVERY_BONUS_TURNS = 2`) — found necessary by live testing: `ExecuteDirect`'s real
     4-turn budget means the nudge fires at turn 3 and removal at turn 4, the *last* turn, leaving
     zero turns to benefit from removal. This is *not* a general "loops are free" refund — opencode,
     kilocode, and VTCode were all checked directly and none of them refund/extend budget just
     because a loop was detected; this is narrower and reactive (triggered by an observed loop, not a
     model's upfront guess at task difficulty), capped at exactly one grant per run.
   - 3rd+: give up honestly with a named diagnosis (`doom_loop_failed_report`/`cycle_failed_report`)
     instead of a generic budget-exceeded message.
3. **Report real progress on budget exhaustion**, even when nothing above fires — a separate live
   gap: a run that made genuine progress (e.g. wrote a vault note) before running out of turns to
   file `submit_report` previously came back as a bare `Failed`, `artifacts: []`, indistinguishable
   from zero progress. `budget_failed_report_with_progress` now returns `PartiallySucceeded` when any
   call actually succeeded, with a compact "tool -> result preview" summary (reusing the existing
   `preview()` truncation) — deliberately *not* the raw tool-call/result trace, which would defeat the
   token-efficiency point of delegating in the first place. `artifacts`/`new_high_signal_facts` are
   left for a human or a future cheap-model summarizer to derive; mechanically guessing which preview
   string is "really" a written path would mean parsing arbitrary tool-specific result text, a
   judgment call, not a mechanical one.

**Live result**: re-running the exact reproduction that started this investigation —
`deepseek/deepseek-v4-flash` (3 samples) + `google/gemini-3-flash-preview` (3 samples) on
`multi-step-research` — went from **0/6 to 5/6** `calls_matched` *and* `outcome_matched`. The one
remaining failure (DeepSeek, 1 sample) called `deepwiki` then `vault` *twice* (genuine progress, no
loop) but didn't file `submit_report` before the (now 6-turn, post-bonus) budget ran out — a timing
gap, not a doom loop; would now correctly report `PartiallySucceeded` with the actual calls named,
not a bare failure.

Full test coverage: 26 tests in `crates/executor/src/lib.rs` (near-duplicate detection, cycle
detection, single-turn-batch duplicates, false-positive avoidance for genuinely distinct queries,
each escalation step, the tight-budget recovery-bonus regression, and both budget-exhaustion outcome
paths). Zero regressions across the full workspace at each step.

## What's still open

1. ~~`multi-step-research` still doesn't land on a clean `Succeeded`~~ — **resolved**, 5/6 above.
2. ~~Whether this is DeepSeek-specific or general~~ — **answered**: not DeepSeek-specific. Gemini
   showed the identical repeat-the-same-tool pattern; a third model tested in the same live run
   (`openai/gpt-5-nano`) did not, completing the goal cleanly every time. Narrows to "some
   fast/cheap-tier models are prone to this," not one vendor.
3. `honest-failure-report`'s earlier regression was not re-investigated this session — still
   unconfirmed whether it was real-model sampling noise or something else.
4. ~~No way to see *why*, currently — only outcomes~~ — **partially addressed**: `run_loop` now logs
   the model's `response.content` alongside tool calls when present. Tested live: DeepSeek produced
   *zero* visible text alongside its repeat calls in this scenario — an actual answer, not just an
   unaddressed gap: for at least this model/scenario, there's no reasoning trace to read even with
   the logging in place, which is itself evidence for why an engine-level guard (rather than hoping
   for an inspectable "why") was the right fix.
5. **New, from this session**: the escalation ladder needs at least 2 strikes' worth of turns
   (nudge + removal) before it can possibly pay off, plus at least 1 more to exploit the removal —
   a *minimum* of ~5-6 turns. A budget tighter than that (nothing currently is, but a future tuning
   change could make one) would make the whole mechanism structurally inert again, the same way the
   bare 4-turn budget did before the recovery bonus was added. Worth a regression test or a runtime
   assertion if `DIRECT_MAX_TURNS`/`DEFAULT_MAX_TURNS` are ever tuned down.
6. **Deferred by design, not forgotten**: a cheap-model summarizer for budget-exhaustion reports (if
   the mechanical "tool -> preview" listing turns out insufficient for a redeploying agent to act on),
   and an ID-addressable "inspect one past tool call in detail" mechanism (deliberately not built —
   risks the redeploying agent recursively drilling into calls one at a time the same way the
   original doom loop couldn't tell when it had enough information; only worth building if a compact
   report is demonstrated insufficient in practice).

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

- Full run-by-run data and narrative (original session): `docs/roadmap/heuristics-tuning-engine-plan.md`'s
  "Executor-layer live smoke tests" and "Subagent-layer" sections.
- The original fix: `crates/executor/src/lib.rs`'s `REPORT_NUDGE`.
- The follow-up session's fixes: `crates/executor/src/lib.rs`'s `is_doom_loop`, `detect_short_cycle`,
  `args_similarity`, `tool_removed_nudge`/`tools_removed_nudge`, `DOOM_LOOP_RECOVERY_BONUS_TURNS`,
  `budget_failed_report_with_progress`.
- The live reproduction tool: `crates/heuristics-tuner/examples/debug_multistep.rs`.
- The scenario definition: `crates/heuristics-tuner/src/tool_scenarios.rs`.
- Independent research that corroborated the fix direction before it was built:
  `docs/ideas/doomloop_research.md`.
- The diagnostic idea for next time: `docs/ideas/model-reasoning-introspection-idea.md` (this
  session's live content-logging test is a direct, partial answer to it — see "What's still open"
  item 4 above).
- Engine architecture: `crates/executor/ARCHITECTURE.md` (not yet updated to describe the new guard —
  worth doing alongside/after this commit).
