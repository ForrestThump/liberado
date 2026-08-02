# Token cost accounting

**Status**: scoped, not built. Written 2026-08-02.
**Purpose**: answer design questions with measurements instead of guesses. The immediate one is
open in [`delegated-work-is-discarded-at-the-seam.md`](delegated-work-is-discarded-at-the-seam.md) —
*what does carrying a research report inline actually cost?* — and it is currently unanswerable.

**Do not re-instrument.** Most of the substrate exists and is live. This is about the four things
sitting on top of it.

## What already exists

`crates/provider/src/latency.rs` records **every inference call** through a role-tagged
`MeteredProvider`, appending JSONL to `<data>/latency/events.jsonl`. Landed 2026-07-20 as slice 1 of
[`latency-and-routing-observability-plan.md`](latency-and-routing-observability-plan.md).

Per call, already captured:

| field | why it matters here |
|---|---|
| `prompt_tokens`, `completion_tokens`, `total_tokens` | the raw quantity to price |
| `cached_prompt_tokens` | cached input is typically ~10× cheaper; ignoring it overstates cost badly |
| `model` | prices are per model |
| `role` | attributes cost to face agent / dispatcher / subagent / orchestrator |
| `correlation` | joins a chat turn to the dispatch work it caused |
| `wall_ms`, `ttft_ms`, `finish`, `tool_calls`, `streamed` | already used for latency |

Live on the box: **1,307 records**. A representative line shows `prompt_tokens: 24455` of which
`cached_prompt_tokens: 20736` — an **85% cache hit**, already measurable and currently unread.

So the measurement problem is solved. The accounting problem is not.

## The four gaps

### 1. There is no money anywhere

Tokens are not cost. Nothing in the system knows that `deepseek-v4-pro` input costs a different
amount from `gemini-2.5-flash-lite` output, or that cached input is a fraction of fresh input.
Without a price table every "expensive" claim is vibes.

### 2. Nothing aggregates

`correlation` makes the join *possible* — a chat turn and the subagent it dispatched share an id —
but nothing performs it. The true cost of a user turn is its own calls **plus every call its
delegation caused**, and today that requires reading JSONL by hand. The existing report script
(`deploy/homelab/latency-report.sh`) does p50/p95 latency per role and no cost at all.

### 3. There is no estimate before spending

Everything above is post-hoc. "Estimate" in the ask means a number *before* the call: this context
is N tokens, on this model that is about $X. That is what makes it a design instrument rather than a
receipt — it can inform a decision, and it can be compared against the actual afterwards.

### 4. It is not surfaced

`/api/status` has `token_usage_total: null` — a stub that has never been filled.

## Deliverables

In order. Each is independently useful; stopping after 2 already answers the question that prompted
this.

### D1 — a price table in config

`[[models]]` entries in `topology.toml` gain optional per-million rates: `input`, `output`, and
`cached_input`. Optional because an unpriced model must degrade to "tokens known, cost unknown"
rather than to a wrong number or a crash.

Config, not code: prices change without warning and nobody should rebuild a daemon for a price cut.
Same reasoning as `[roles.*]` model selection.

**Done when**: a model with no price entry reports tokens and a `null` cost, and nothing infers a
default rate.

### D2 — a cost query over the journal

A tool (extend `liberado-eval`'s shape, or a subcommand) that reads `events.jsonl` and answers:

- cost per **conversation**, rolled up through `correlation` to include dispatched work
- cost per **role**, so "the dispatcher is 40% of spend" is a fact
- cost per **turn**, and how a conversation's prompt tokens grow turn over turn
- **cache hit rate**, since the 85% above is the difference between a scary number and a fine one

That last two are what settle the inline-findings question: if carrying a report inline pushes
prompt tokens up ~8k per subsequent turn, the cache rate decides whether that is nearly free or
compounding.

**Done when**: one command prints per-conversation cost including delegated work, against the real
journal on the box.

### D3 — pre-flight estimate

Before a turn, the assembled context is already known. Multiply by the input rate, add a completion
allowance, log it alongside the call. The recorded actual then sits next to the estimate, so the
estimator's error is itself measurable — an estimator nobody checks drifts.

**Done when**: `LatencyEvent` carries `estimated_cost` beside the actual, and the query in D2 can
report estimator error.

### D4 — surface it

Fill `token_usage_total`, and add cost to the conversation header a surface can show. Last on
purpose: a number on a screen invites tuning, and tuning before D2 is guessing with extra steps.

## Design decisions worth stating up front

**Record tokens, compute money at read time.** Prices change retroactively for old records if they
are baked in at write time, which makes historical comparison lie. The journal stays a record of
*what happened*; pricing is applied by the query.

**Cost is a property of the correlation tree, not a call.** A user turn that delegates is one
question and many calls. Anything that reports per-call cost without a rollup will systematically
understate the expensive path — which is the exact path worth optimising.

**Absent price ≠ zero.** The most likely way this misleads is a new model slug appearing with no
entry and silently costing nothing. `Option<f64>` throughout, and the query reports unpriced calls as
a separate line rather than folding them into a total.

**Do not add a second journal.** The temptation is a `cost.jsonl`. Two records of the same event
drift, and §6 of [`failure-modes.md`](../spec/architecture/failure-modes.md) is exactly that shape.
Extend `LatencyEvent`.

## What this is not

Not billing, not quota enforcement, not a budget that refuses work. Those need accuracy guarantees
this will not have — provider-reported usage is itself approximate, and streaming calls sometimes
report none at all. This is an instrument for design decisions.

## First question to point it at

Whether `relay_directive` (landed 2026-08-02) made delegated turns materially more expensive. It
asks subagents to return findings rather than a status line, which is strictly more tokens into the
face agent's context, on every later turn of that conversation. The change is right regardless — the
alternative was a fabricated answer — but "how much did honesty cost" should be a number, and right
now it cannot be one.
