# Token economics — where the tokens actually go (measured 2026-08-02)

**Status:** findings + three work items. Nothing here is built yet.

The first thing `liberado-cost` was built for was to stop guessing about spend. This is the first
real read of it, over the deployed journal at build `e85a0eb`: **1,338 inference calls, 15,496,141
tokens, 193 conversations, 469 turns, 12 configured MCPs.**

Every number below is from that journal. The scripts are one-offs; the reusable one is
`liberado-cost delegation-cost` ([`crates/cost/src/lib.rs`](../../crates/cost/src/lib.rs)).

---

## The headline

| role | tokens | share |
|---|---|---|
| orchestrator | 14,373,872 | **92.8%** |
| face | 693,463 | 4.5% |
| dispatcher | 428,806 | 2.8% |

And within the orchestrator's 13,953,296 prompt tokens:

| | tokens | share of orchestrator | share of **everything** |
|---|---|---|---|
| re-sending the run's base context on every hop | 8,730,597 | 62.6% | **56.3%** |
| accumulation inside the loop (tool results) | 5,222,699 | 37.4% | 33.7% |

**Fifty-six percent of every token this system has ever spent is the same ~11k base context, re-sent
on each hop of each run.** That is the single largest line item by a wide margin, and it is the one
nobody was looking at.

---

## How the roles actually behave

Per call, which is where the intuitions break:

| role | calls | median prompt | max prompt | mean tool calls |
|---|---|---|---|---|
| dispatcher | 256 | 1,499 | **3,677** | **0.00** |
| face | 221 | 1,282 | 14,796 | 0.52 |
| orchestrator | 861 | 13,674 | **96,108** | 2.15 |

The dispatcher is *not* expensive, and the reason is worth stating because it is easy to assume
otherwise: it never receives tool schemas. Its prompt is built at
[`crates/dispatcher/src/lib.rs:233`](../../crates/dispatcher/src/lib.rs#L233) as one line of
`- {name}: {description}` **per MCP server**, plus the goal string — no per-tool entries, no JSON
schemas, and no conversation history. `mean tool calls = 0.00` across all 256 calls: it emits one
structured decision and stops. It never loops, and its prompt is bounded at 3,677 tokens.

The orchestrator is the opposite on every axis, and it is where the tool schemas live.

### Orchestrator runs are not one-shot

191 runs: median **4 hops**, mean 4.5, max 21. Only **8** runs were a single hop; 110 were 4–8.

### Under half of it is delegation

By correlation prefix: `chat` 94, `vault` 58, `cron` 22, other 7. Roughly **40% of orchestrator spend
is autonomous reactive and scheduled work** that would exist with no chat surface at all. It is not
delegation overhead and cannot be optimised away by changing how chat delegates.

### The base is flat, which is what identifies it

Median first-hop prompt, bucketed by how long the run turned out to be:

| hops | runs | median base | median total | if base were the whole story (N × base) | accumulation |
|---|---|---|---|---|---|
| 1 | 8 | 10,942 | 10,942 | 10,942 | 0 |
| 2–3 | 54 | 11,050 | 25,864 | 22,100 | 3,764 |
| 4–8 | 110 | 10,970 | 57,730 | 43,880 | 13,850 |
| 9+ | 19 | 10,963 | 222,896 | 109,630 | 113,266 |

Goals vary enormously in length; a base that lands within 108 tokens across all four buckets is a
**fixed structural payload** — system preamble plus tool schemas — not anything derived from the
task. With 12 MCPs configured, ~11k is about what the full catalog costs.

**The two terms cross over around 9 hops.** Below that, re-sending the base dominates; above it,
accumulated tool results do.

---

## What this corrects

**"Delegation reduces context bloat."** True, and measured: paired within-conversation and
controlled for turn position (delegating and non-delegating turns sit at mean index 3.16 vs 3.11),
the face's context grows by a median **+38 tokens** after a delegating turn versus **+2,949** after a
non-delegating one. The mechanism works exactly as designed.

It is just attached to a small problem. Face context is 4.5% of spend, the saving is discounted
further by a 76.4% cache hit rate on carried context, and it is only collectable across later turns
— of 185 conversations, **136 are a single turn** and only 13 reach four. Break-even for a
delegating turn against inline work lands around 34 follow-up turns.

**"The quadratic cost is why we isolate the orchestrator."** The quadratic term is real — it is the
accumulation row — but it is the *minority* term below 9 hops, and **isolation does not touch it**:
tool results accumulate inside a subagent exactly as they would inside the face. What isolation
actually buys is the *linear* term: every token kept out of the base is saved once per hop, a 4.5×
multiplier at the median. Valuable, and worth keeping — but linear, and aimed at an 11k payload that
is mostly tool schemas rather than inherited conversation.

---

## TE1 — find out why the tool catalog isn't being narrowed

**The symptom is established; the cause is not.** Do not start by building narrowing — it already
exists on both dispatch paths:

- `ExecuteDirect.relevant_mcps` — intersected against the grant in
  [`crates/orchestrator/src/lib.rs:856`](../../crates/orchestrator/src/lib.rs#L856), and
  `DispatchTuning::narrow_direct_tools` defaults **`true`**.
- `DispatchSubagent.allowed_mcps` — passed to `runtime_for` and used to derive the risk gate. It is
  in the classifier's JSON schema and listed in `"required"`, so the model is obliged to emit it.

Yet the base is a flat ~11k, which is roughly the full 12-MCP catalog. Something between "the model
emits a list" and "the executor sends schemas" is not narrowing. Candidates, none confirmed:

1. The classifier emits all (or nearly all) MCP names, so the intersection is a no-op.
2. Narrowing works, but surviving MCPs carry large enough schemas that it barely moves.
3. An empty `allowed_mcps` means "everything" to `RuntimeFactory`/`ScopedRuntime` — the inverted
   sense already flagged in the `ExecuteDirect` comment — and subagent dispatches are hitting it.
4. The 11k is dominated by the fixed preamble rather than schemas, in which case narrowing is the
   wrong lever entirely and the preamble is the target.

**First step is instrumentation, not a fix.** `allowed_mcps.len()` is logged at `debug`
([`orchestrator/src/lib.rs:865`](../../crates/orchestrator/src/lib.rs#L865)) and the box runs at
`info`, so this could not be answered from the outside. Promote a count — offered vs chosen vs
surviving MCPs, and the resulting schema token size — to `info` or onto the dispatch journal, run a
day, and *then* pick the fix.

**Why it matters:** this is 56% of all token spend. Nothing else on this page is close.

## TE2 — split subagent from direct-execution spend

`AgentRole` is `Face | Dispatcher | Orchestrator | Unknown`
([`crates/provider/src/latency.rs:29`](../../crates/provider/src/latency.rs#L29)). **Delegated
subagent runs and `ExecuteDirect` both journal as `orchestrator`**, and nothing in the journal
separates them — the dispatch journal records `start` and `disposition` but not the decision type.

So the 92.8% is a merged bucket, and questions like "is a subagent more expensive than doing it
directly?" or "which routing decision costs more?" are currently unanswerable. Note this is
precisely the design question the dispatcher exists to make well.

Either add a role (`Subagent`) or record the decision type on the dispatch journal's start record.
The journal's shape is a contract with a reader — `crates/cost/tests/journal_shape.rs` guards it —
so a writer change needs that test updated in the same PR, and the reader tolerates unknown fields
already.

**Landmine:** `crates/provider/src/latency.rs` was deliberately owned by nobody in the round-2
parallel work. Changing it is its own PR, not a side effect of another.

## TE3 — order the dispatcher prompt so its stable part can cache — ✅ **landed**

**Shipped; verified on `main` 2026-08-10.** `build_request` in
[`crates/dispatcher/src/lib.rs`](../../crates/dispatcher/src/lib.rs) now assembles stable-first —
catalog, then writable zones, then the varying goal — with a comment at the site explaining why the
order is load-bearing so a future edit does not undo it.

**The effect was never re-measured.** The predicted gain is ~1% of total tokens; nobody has
confirmed the dispatcher's cache-hit rate actually moved. If you want the number, that is a fresh
`liberado-cost` read, not a code change.

The original finding follows, kept because the reasoning generalises to any prompt we assemble.

---

Dispatcher cache hit was **22.3%**, against 76.4% (face) and 76.7% (orchestrator).

The cause is visible in one line
([`crates/dispatcher/src/lib.rs:240`](../../crates/dispatcher/src/lib.rs#L240)):

```rust
let mut user_message = format!("Goal:\n{}\n\nAvailable MCPs:\n{}", req.goal, catalog);
```

The **goal varies every call and is placed first**; the MCP catalog, zone list, and guidance are
stable and sit after it. Prefix caching only matches a stable prefix, so the varying goal poisons
everything downstream. The only cacheable region left is the fixed system prompt — about 22% of a
1,499-token prompt, which is exactly the hit rate observed.

Fix: stable content first (catalog, zones), varying goal last. It is a format-string reorder.

**Be honest about the size:** the dispatcher is 2.8% of spend and this converts perhaps half its
prompt from full rate to cached rate — on the order of 1% of total tokens. It is on this list
because it is nearly free, because it is *correct*, and because the same ordering mistake in a hot
prompt would cost real money. Check the orchestrator's prompt for the same shape while you are
there — that one is 92.8%.

---

## Order to do them in

**TE1 first, and it starts with a measurement, not a change.** TE2 makes TE1's results legible and
is cheap. TE3 is a one-line reorder that can ride along with anything.

Resist the temptation to start with TE3 because it is the easiest to write.

---

## Caveats

- **Cache is not modelled in the token split.** The base is precisely the stable prefix, so it is
  the *most* cached part of every prompt; in dollar terms its 62.6% share is discounted well below
  that, while accumulation — where the newest tool result is never cached — costs more per token
  than its share suggests. The token ranking here is solid; the dollar ranking would be flatter and
  has not been measured. Doing so needs `[[models]]` rates, which the box does not declare — see
  [`tuning.md`](../spec/reference/tuning.md).
- **"Delegating turns cost 11× more" is confounded** by task difficulty: the dispatcher routes hard
  work to subagents, and those tasks might have cost as much inline. The face-context finding is
  paired and position-controlled; this one is directional only.
- **12.9 days, one operator, one deployment.** Directions are clear at this sample size; precise
  magnitudes are estimates.
