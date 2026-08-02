# Idea: turn budgets sized per model, not per role

**Status:** idea, captured 2026-08-01. Not scheduled. Related:
[`turn-budget-battery-idea.md`](turn-budget-battery-idea.md) (how a budget *behaves* when it runs
out — this is about how big it should have been), and CH4 per-conversation models in
[`../../roadmap.md`](../../roadmap.md).

## The observation

Switching the daemon's model from `deepseek/deepseek-v4-pro` to `z-ai/glm-4.5-air` for dogfooding
also switched the **research subagent**, because only `[roles.dispatcher]` declares an explicit
model and everything else inherits the provider default. That run then hit its ceiling:

```
budget exhausted on salvageable work; granting wrap-up reserve  turn=9  reserve=3
outcome=PartiallySucceeded
```

The report landed, but partial. The budget was 8 turns — a number chosen when a stronger model was
answering.

## The idea

A turn budget is a proxy for "how many steps should this take", and that is not a property of the
*role*. It is mostly a property of the **model**: a cheap non-thinking flash model plausibly needs
more steps to arrive where a reasoning model gets in fewer, because it does less per step. Sizing
the budget per role and holding it fixed across models means every model swap silently re-tunes how
much work the system can finish, in the direction nobody chose.

Shape, if built: an optional per-model turn multiplier or absolute override, resolved the same way
CH3's compaction trigger resolves per-model overrides — declared beside the model, not beside the
role.

```toml
[[models]]
name = "z-ai/glm-4.5-air"
# ... existing fields ...
turn_budget_scale = 1.5   # or an absolute turn_budget
```

## Why it is only an idea

* **One data point.** One partial report on one research goal is not evidence that GLM 4.5 Air is
  turn-hungry; the goal may simply have been large. Worth a second and third observation before
  turning a hunch into config.
* **It may be the wrong lever.** If the real problem is that a weaker model wastes turns on
  redundant calls, more turns buys more waste. The redundant-tool-call note in
  [`../../roadmap.md`](../../roadmap.md) (§Cross-cutting, 2026-07-28) suggests that failure exists
  independently — four `list_events` calls for two dates, absorbed by the doom-loop guard.
* **It interacts with cost.** The reason to run a cheap model is spend; scaling its budget up gives
  some of that back. Whether the trade is worth it is an empirical question, not a design one.

## What would make it decidable

The latency journal ([`../latency-and-routing-observability-plan.md`](../latency-and-routing-observability-plan.md))
already records per-run data. Turns-to-completion, bucketed by model, over a handful of comparable
goals would answer this directly — and per-node model provenance (CH4 follow-on) is what makes that
bucketing possible at all, since today nothing records which model produced which turn.

**Do not build this before that data exists.** The number would be a guess wearing a config key.
