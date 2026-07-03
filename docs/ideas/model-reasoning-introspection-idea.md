# model-reasoning-introspection-idea.md — Ask *why*, don't just guess from outcomes

**Status**: Idea, not decided. Captured 2026-07-06 during live executor-layer tuning
(`docs/roadmap/heuristics-tuning-engine-plan.md`'s "Executor-layer live smoke tests" section) so it
isn't lost — nothing here is scheduled or designed in detail yet.

**Goal**: When a model makes a choice we didn't want (wrong tool call, wrong final outcome, an
unsafe act), stop inferring *why* purely from aggregate pass/fail stats — ask the model directly, or
read what it already told us.

## Where this came from

Debugging `multi-step-research`'s repeated failures (`liberado-heuristics-tuner`'s executor-layer
scoring) meant reasoning backward from three booleans (`calls_matched`/`unsafe_call`/
`outcome_matched`) and a hunch about `Executor::run_loop`'s nudge mechanism — a hunch that turned out
right (`REPORT_NUDGE`'s wording was biasing against multi-step plans), but confirming it required
reading engine source code and reasoning about turn-by-turn behavior speculatively, since nothing
anywhere captures what the model actually said/thought turn-to-turn. A live run would have settled
it in one shot.

## Two concrete mechanisms

1. **Configurable interjection on an undesired choice.** When a scored trial lands on an outcome we
   didn't want (a scenario's `must_not_call`/`must_call`/`expected_outcome` mismatch, or an
   eval/dispatcher misroute), optionally fire one more turn against the *same* conversation history
   asking the model something like "explain your reasoning for the choice you just made" — not to
   change its answer, purely to capture an explanation alongside the trial result. Configurable (a
   session opts in, since it's an extra call per flagged trial) and could apply to the dispatcher's
   `liberado-eval`/tuner scoring, the executor/subagent tuner scoring, or even a real production
   trace when a human is investigating a specific misroute after the fact.
2. **Read the reasoning that's already there, for thinking-capable models.** Some providers/models
   expose a reasoning/thinking trace alongside the final response (distinct from the visible
   content) — `liberado-provider`'s `CompletionResponse` doesn't currently surface anything like
   this (checked 2026-07-06: no `reasoning`/`thinking` field anywhere in `crates/provider/src`).
   Surfacing it, where the underlying API provides it, would explain a choice with zero extra calls
   — strictly better than (1) when available, since it's not an extra round-trip and reflects the
   actual reasoning that produced the answer, not a post-hoc reconstruction of it.

## Where this would plug in, if built

- `liberado-provider`'s `CompletionResponse` would need an optional reasoning/thinking field (only
  populated when the underlying provider exposes one — OpenRouter passes through several providers'
  raw responses, so this is plausibly just wiring, not a new capability to build from scratch).
- The interjection mechanism is a new, small piece of logic layered on top of an existing scoring
  loop (`liberado-eval`'s scoring, or `liberado-heuristics-tuner`'s `score_one`/`score_candidate`
  functions) — fire the extra turn only for trials that already failed some check, not universally
  (keeps it cheap and opt-in).
- Immediate value is diagnostic (understanding *why* a tuning candidate or eval scenario failed,
  without hand-reasoning about engine internals) — not proposed as a runtime/production behavior
  change.

## Not decided yet

- Whether this belongs in `liberado-eval`, `liberado-heuristics-tuner`, or both (they'd likely want
  the same mechanism, similar to how `tool_loop_scoring.rs` was kept parallel to `scoring.rs` rather
  than shared, until there's a second real consumer to justify unifying).
- Whether the interjection's own response should just be logged/printed, or become a structured
  field on `ScoredScenario`/`ToolLoopScoredScenario` (richer, but a bigger design commitment).
- Whether "thinking trace" support is worth building generically into `Provider`/`CompletionResponse`
  now, or only once a specific provider/model in active use actually exposes one worth reading.
