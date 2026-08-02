# A delegated subagent's work is discarded at the seam

**Status**: finding, not yet fixed. Written 2026-08-02.
**Severity**: the user gets a confident, cited answer whose specifics the system never researched.

## What happens

`delegate` hands the face agent **only `result.summary`** ([`face.rs:124`](../../crates/main-agent/src/face.rs#L124)). The subagent's actual findings are never passed on. The face agent then writes the user-facing answer from that summary alone.

Measured on a real turn (conversation `01KZ0JQJ5V359744Y3Q2M5RGXC`, 2026-08-02):

| | |
|---|---|
| delegate tool result | **504 chars**, ~a third of it session id and dispatch-journal path |
| its content | *"Comprehensive 11-point comparison of belt drive vs chain drive bicycles… Synthesized from authoritative sources including CyclingAbout (135,000km testing), BikeRadar, Hackaday"* |
| face agent's answer | **7,872 chars** — sectioned, specific, citing those same sources |

The face agent had the *names* of the sources and none of their content. The specifics — carbon belts, eccentric bottom brackets, the 135,000 km figure applied to particular claims — came from its own priors, presented as the outcome of research the system genuinely performed and then threw away.

**Be precise about what this does and does not prove.** The content may well be accurate; belt-vs-chain is well-trodden. What is false is the **provenance**: the answer asserts it was synthesised from sources nothing in the pipeline read. That is harder to catch than a thin answer, because it reads better. A visibly thin answer would at least announce the problem.

## It is not a model-quality problem

Reproduced twice, with different face models:

- 2026-08-01, fuel injectors — face `google/gemini-2.5-flash-lite`, subagent `deepseek/deepseek-v4-pro`
- 2026-08-02, belt drive — face **and** subagent `deepseek/deepseek-v4-pro`

Same shape both times, including on the strongest model available. Matching models is not needed to reproduce, and "use a better model" is not a fix.

Surviving evidence: subagent session `01KYZTWH9CFMNA7CJ3NNJCSE55`, and conversation `01KZ0JQJ5V359744Y3Q2M5RGXC` in full. (The 08-01 parent conversation was deleted.)

## Two separable defects

Conflating these wasted time, so they are named apart:

| | |
|---|---|
| **Payload** | `result.summary` is a *description of* the work, on every path. Nothing carries the findings. **This is the bug.** |
| **Routing** | who relays the result to the human — the face agent, or the daemon writing straight into the chat |

Fix routing alone and a thin answer is delivered more efficiently. Fix payload alone and the face agent stops fabricating. **Only the payload fix is load-bearing.**

## Routing: decided, and the decision is "leave it"

There is existing machinery for direct delivery. **S4's return handoff** (landed 2026-07-12) has a server-side watcher, `spawn_return_handoff`, that calls [`ChatSessions::append_note`](../../crates/main-agent/src/sessions.rs) on terminal and writes the result into the parent conversation as an `Author::Named("goal-session")` node — no face agent, no dispatcher, no tokens. It is used by `/spawn`. `delegate` does not use it.

Pointing `delegate` at it was considered and **rejected** (Shiloh, 2026-08-02):

- The token saving is small — summaries are not large — so it buys little.
- It is conversationally clunky. A report is written *to the delegator*, as a status; pasting that into a chat relocates the problem rather than solving it.
- The face agent holds context the subagent does not. It should decide whether to relay near-verbatim or trim, based on what the conversation already covered.

That last point is the actual argument, and it **requires** the payload fix: a face agent given a 504-char status line cannot choose to relay it verbatim, because there is nothing there to relay.

## The tension the fix has to resolve

The face agent needs the findings, and the findings can be long. Putting a full report inline in a tool result spends the face agent's context on every subsequent turn and pulls compaction (CH3) forward.

Three shapes, none obviously right:

1. **Inline full content.** Simplest, grounds the answer, worst for context.
2. **Artifact + pointer.** The subagent writes a note (it already has a vault grant) and the tool result carries a summary plus the path. The face agent reads it if it needs detail. Cheap, but adds a read the model may skip — and skipping it returns to today's behaviour silently.
3. **Summary plus findings block.** Structured result: a short status *and* the substantive content, with the content elided from history after the turn that used it.

Whichever is chosen, `report` mode needs a **stated contract** for what a summary is. Today nothing says whether it is a status line or the deliverable, which is why it is a status line.

## What must not regress

Today's durable-turn work touched code this fix will brush against:

- **`append_note` authors as `Named("goal-session")`, not `Assistant`.** Deliberate. Model derivation (`model_last_used`) filters on `Author` precisely so a delegation cannot capture the conversation's model, and `last_turn_unanswered` treats a `Named` node as ending the turn. Making a handoff look "as if the face agent said it" by authoring it `Assistant` would change both. Check them together.
- **The tool node carries no model stamp.** An MCP produced it, not a model. Any change to how results are recorded must keep that true.

## Root cause (found 2026-08-02)

The contract asks for exactly what we got. `submit_report`'s schema:

```json
"summary":   { "description": "High-signal, human-readable, short." },
"artifacts": { "description": "Vault paths written, e.g. \"reviews/2026-06-21.md\"." }
```

**"short."** The model complied. And `artifacts` is for *vault paths*, so a research task that writes
no file has nowhere to put findings even if it wanted to.

**The fix already exists, and does not run on this path.** `orchestrator::delivery_directive` tells a
subagent its summary *is* the document — "complete, structured with markdown headings, as long as the
material warrants, carrying the detail, sources and specifics you gathered". Its own doc comment
diagnoses this exact failure, from a live run that filed 231 bytes reading *"I have all the research
I need. Let me now write the comprehensive report directly to the vault."*

It is appended only when `delivery_target` yields a path:

```rust
let path = match requested {
    Delivery::Summarize => return Err(None),   // chat delegation lands here
    Delivery::Vault { path } => path,
};
```

A chat `delegate` is `Delivery::Summarize`. No path, so no directive, so the default "short" contract
stands. The scheduled-report path gets the good contract; `delegate` never did.

This is the **second** mechanism found this way. `append_note` (direct delivery) is used by `/spawn`
and not by `delegate`; `delivery_directive` is used by vault delivery and not by `delegate`. The
pattern is not a coincidence worth ignoring: **`delegate` is the path improvements keep missing**,
probably because it is the one with no durable artifact to inspect afterwards.

## The fix, in shape

`Delivery::Summarize` needs its own directive. The destination differs — a conversation rather than a
file — but the contract is identical: *the summary is the material, not a status*. The coupling to be
broken is that the directive currently keys on **having a file path** when what actually matters is
**whether the summary is the deliverable**.

`is_read_only_dispatch` already distinguishes research from action and is used a few lines away to
pick the budget and loop profile, so the gate exists.

Note this keeps Shiloh's routing decision intact: the face agent still receives the material and still
decides whether to relay it near-verbatim or trim it to the conversation. It simply receives material
instead of a status line — which is what makes that choice possible at all.

**Still open**: the context cost. A full research report inline in a tool result occupies the face
agent's history on every later turn. See the three shapes above; that trade-off is not resolved by
knowing the root cause.

## Next step

Read what the dispatch pack actually asks a subagent to return, and whether `report` mode states a contract at all — before touching prompt wording. The answer to "why is the summary a status line" is probably written down in the pack, or conspicuously not written down anywhere.
