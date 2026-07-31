# model-reasoning-introspection-idea.md — Read real reasoning; don't mistake rationalization for it

**Status**: Idea, not decided. Captured 2026-07-06 during live executor-layer tuning
(`docs/future-work/heuristics-tuning-engine-plan.md`'s "Executor-layer live smoke tests" section) so it
isn't lost — nothing here is scheduled or designed in detail yet. **Revised same day**: the two
mechanisms below are not equally sound — see the caveat under mechanism 1.

**Goal**: When a model makes a choice we didn't want (wrong tool call, wrong final outcome, an
unsafe act), stop inferring *why* purely from aggregate pass/fail stats. Only mechanism 2 below
(reading an actually-exposed reasoning trace) gives a real answer; mechanism 1 (asking afterward) is
weaker than first captured and shouldn't be treated as introspection.

## Where this came from

Debugging `multi-step-research`'s repeated failures (`liberado-heuristics-tuner`'s executor-layer
scoring) meant reasoning backward from three booleans (`calls_matched`/`unsafe_call`/
`outcome_matched`) and a hunch about `Executor::run_loop`'s nudge mechanism — a hunch that turned out
right (`REPORT_NUDGE`'s wording was biasing against multi-step plans), but confirming it required
reading engine source code and reasoning about turn-by-turn behavior speculatively, since nothing
anywhere captures what the model actually said/thought turn-to-turn. A live run would have settled
it in one shot.

## Two mechanisms — one real, one weaker than it first looked

1. **Configurable interjection on an undesired choice — a post-hoc rationalization, not a report.**
   The original framing here was "ask the model what it was thinking," which is the wrong way to
   describe it and was called out as such (2026-07-06): a non-thinking model has no persistent
   record of "what it was thinking" to query. Firing one more turn asking "explain your reasoning
   for the choice you just made" gets a *freshly generated, plausible-sounding explanation*
   conditioned on the conversation so far — not a retrieval of the actual reasoning that produced
   the original output. This is the well-documented "unfaithful explanation" problem in LLM
   interpretability: a stated justification isn't guaranteed to match what actually drove the
   original generation, and models can confidently confabulate. **Real, limited value it still has**:
   the confabulated explanation can still reveal how the model *appears* to be interpreting the
   task (e.g., "I thought the research step alone completed the goal" is informative about an
   apparent misunderstanding even if it's a reconstruction, not a trace) — but it must be treated as
   a soft, unreliable signal, never as ground truth, and never described as "what it was thinking."
2. **Read the reasoning that's already there, for thinking-capable models — the actually sound
   mechanism.** Some providers/models expose a reasoning/thinking trace alongside the final response
   (distinct from the visible content), generated as part of the *same* forward pass that produced
   the answer — causally connected to it, not a separate after-the-fact query. `liberado-provider`'s
   `CompletionResponse` doesn't currently surface anything like this (checked 2026-07-06: no
   `reasoning`/`thinking` field anywhere in `crates/provider/src`). Surfacing it, where the
   underlying API provides one, is strictly better than (1) when available — zero extra calls, and a
   real answer instead of a plausible-sounding guess.

## Where this would plug in, if built

- `liberado-provider`'s `CompletionResponse` would need an optional reasoning/thinking field (only
  populated when the underlying provider exposes one — OpenRouter passes through several providers'
  raw responses, so this is plausibly just wiring, not a new capability to build from scratch).
- The interjection mechanism (1) is a new, small piece of logic layered on top of an existing
  scoring loop (`liberado-eval`'s scoring, or `liberado-heuristics-tuner`'s
  `score_one`/`score_candidate` functions) — fire the extra turn only for trials that already failed
  some check, not universally (keeps it cheap and opt-in). Whatever surfaces it (a log line, a
  rubric section) must label it as a rationalization/soft signal, not "the model's reasoning."
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
