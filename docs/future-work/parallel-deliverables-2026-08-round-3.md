---
kind: plan
status: active
authority: implementation
domain: process
canonical_for: parallel-deliverables-r3
open_items: true
---

# Round 3 — status and remaining work

Written 2026-08-02. **Rewritten 2026-08-03** after §1 closed, to be executable by someone who has
not read the rest of this repo.

## Status

| # | Deliverable | State |
|---|---|---|
| 1 | Delegated findings reach the face | ✅ **Done.** Payload fix landed `e0fde79` (2026-08-02); boundary tests PR #39 (2026-08-03). One residual, pinned and deliberately unfixed — see below. |
| 2 | Subagent vs direct execution in the journal | 🔴 **Open — do this first.** Spelled out below. |
| 3a | Measure redundant tool calls | 🟡 **Open, delegable.** |
| 3b | Stop paying for redundant tool calls | 🔒 **Reserved** — safety judgement, not delegated. |

**§1's residual:** `ExecuteDirect` gets no output contract, and its `DIRECT_INSTRUCTIONS` ask for a
*"concise, high-signal result"*. Pinned by `execute_direct_gets_no_output_contract_today` and left
unfixed on purpose — `ExecuteDirect` carries no `Delivery`, so a blanket fix would tell every cron
and vault-triggered run to write documents, and that is 92.8% of token spend. Reasoning in
[`delegated-work-is-discarded-at-the-seam.md`](archive/delegated-work-is-discarded-at-the-seam.md). **Do not
"fix" this without reading that section.**

---

## Before you start anything on this page

1. **`git log` the area and read the code.** §1 was specced against a doc header that said "not yet
   fixed" a day after it was fixed. That cost a planning round. **A status line is a claim, not
   evidence** — and so is a doc comment (see the `MeteredProvider` warning in §2).
2. **Run the whole suite first**, so you know what green looks like before you touch anything:
   `cargo test --workspace`.
3. **Gates before the PR** — all three, all clean:
   ```
   cargo fmt --all --check
   cargo test --workspace
   cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings
   ```

### The test rules, as a checklist

Rounds 1 and 2 produced ten PRs. **Every single one shipped a test that claimed more than it
checked.** Before opening a PR, go through this:

- [ ] **Run the mutation you claim.** If a doc comment says "deleting X fails this", delete X, run
      it, and paste the output. Three round-2 branches stated a mutation; two were wrong when run.
- [ ] **Would my fixture fail the wrong implementation?** Write down the wrong version you are
      excluding, then check. A one-event fixture cannot tell a running total from a current value; a
      single-correlation fixture cannot tell per-call from per-turn grouping. Both shipped.
- [ ] **Am I asserting the observable end state, or an internal signal?** What a function returned is
      not what got persisted. What the code attempted is not what a user finds after a restart.
- [ ] **Does my test read a file it is contained in?** `include_str!` on your own module makes a test
      unfalsifiable. One shipped.
- [ ] **Does the test name match what it proves?** If a mock returns a scripted answer regardless of
      its input, the test cannot prove anything about what *caused* that answer. Say so in the doc
      comment rather than naming the test after what you wish it proved.
- [ ] **No substring assertions on rendered output or serialized blobs.** Match parsed structure.

**An honest narrow test beats a broad claim.** "This exercises the query, not the turn" is a fine doc
comment. Pretending otherwise is the defect.

---

## 2. Make subagent and direct execution distinguishable in the journal

**The problem.** 92.8% of every token this system spends is journaled with `role: "orchestrator"`,
and that one bucket merges two different things: work a **delegated subagent** did, and work the
orchestrator did **directly** (`ExecuteDirect`). So *"is delegating cheaper than doing it
directly?"* — the question the dispatcher exists to answer well — cannot be answered at all. Every
token decision after this one is guesswork until it is fixed.

### The approach is decided. Implement this one.

Do **not** design an alternative. Two others were considered and rejected. If you think you have
found a better one, say so in the PR and implement this anyway.

**Add a fourth `AgentRole` variant, and a second metered provider instance for the subagent path.**

**Step 1 — the variant.** [`crates/provider/src/latency.rs:29`](../../crates/provider/src/latency.rs#L29):

```rust
pub enum AgentRole {
    Face,
    Dispatcher,
    Orchestrator,
    Subagent,   // add this
    Unknown,
}
```

plus its arm in `as_str()`, returning `"subagent"`.

> ⚠️ **`MeteredProvider`'s doc comment is wrong.** It says *"role/correlation come from the
> task-local `scope` at each seam"*. Only `CORRELATION` is task-local
> ([latency.rs:47](../../crates/provider/src/latency.rs#L47)); **`role` is bound at construction**,
> and no role scope exists. Fix that comment while you are in there. Do not build on it.

**Step 2 — the second instance.** [`crates/bootstrap/src/lib.rs:203`](../../crates/bootstrap/src/lib.rs#L203)
builds one provider for all orchestrator work:

```rust
subagent: Some(role_provider(ModelRole::Subagent, AgentRole::Orchestrator)),
```

Build a second one tagged `AgentRole::Subagent` and hand it to the `Orchestrator`. Same model, same
config — **only the role label differs.**

**Step 3 — use it at exactly the subagent call sites.** In
[`crates/orchestrator/src/lib.rs`](../../crates/orchestrator/src/lib.rs), five places execute:

| line | what | role after this change |
|---|---|---|
| 871, 879 | `ExecuteDirect` | `Orchestrator` — **unchanged** |
| 968 | `DispatchSubagent` | **`Subagent`** |
| 1205 | `execute_approved_subagent` (the human-approval path) | **`Subagent`** |
| 1275 | parallel sub-dispatch, inside a `tokio::spawn` | **`Subagent`** |

Line 1275 is *why* this approach was chosen over a task-local: a `tokio::spawn`ed child does **not**
inherit task-locals, and we have already shipped that bug once. A constructor-bound provider cannot
have it.

**Step 4 — the reader.** `liberado-cost` must report the two separately. `crates/cost` groups by the
`role` string, so a new value should mostly flow through; confirm `crates/cost/src/report.rs` renders
it and that `crates/cost/tests/journal_shape.rs` still passes.

### Acceptance

- [ ] A delegated subagent call journals `role: "subagent"`; an `ExecuteDirect` call still journals
      `role: "orchestrator"`. **One fixture asserts both** — a fixture of only subagent calls would
      pass an implementation that labels everything `subagent`.
- [ ] The approval path (1205) and parallel sub-dispatch (1275) are covered. The spawn case
      especially: assert the role recorded by work done **inside** the spawned task.
- [ ] Cron- and vault-triggered runs still journal correctly. They reach the orchestrator through the
      same branches, so it should follow — assert it, do not assume it.
- [ ] `crates/cost/tests/journal_shape.rs` passes, and a record written before this change still
      parses (the reader defaults unknown fields — prove it with a fixture line).
- [ ] `liberado-cost` shows the two roles on separate lines.
- [ ] **Run it against real data and put the numbers in the PR.** This is the entire point:
      ```
      cargo run -p liberado-cost -- --data-dir <data-dir>
      ```
      What share of that 92.8% is delegation versus direct execution? Nobody knows. Your PR should be
      the first place that number appears.
- [ ] Workspace green.

### Landmines

- **Do not change what existing records mean.** `face`, `dispatcher` and `orchestrator` records
  already on disk must keep parsing and keep their current labels. You are adding a new label going
  forward. **Say plainly in the PR** that `orchestrator` records written *before* this change contain
  both kinds and cannot be split retroactively — the journal is append-only and history is never
  rewritten.
- **Do not touch `crates/main-agent/src/dispatch_journal.rs`.** It looks relevant and is not: it is
  written by the face's `delegate` tool, so it only ever sees chat delegations — 94 of 191
  orchestrator runs. Cron (22) and vault reactions (58) never appear in it.
- **Do not change model, temperature, or any other setting** on the new provider instance. One label
  differs. If the two can drift in behaviour, the measurement is worthless.

---

## 3a. Measure redundant tool calls

**Do §2 first.** Start this only once §2 is open as a PR.

**The problem.** Of the orchestrator's 13.95M prompt tokens, **37.4% is accumulation inside the agent
loop** — tool results piling up hop over hop. There is a documented instance: a live
`evening-debrief` run called `liberado-caldav-mcp:list_events` **four times for two dates** before the
doom-loop guard fired. The run *succeeded*, which is the problem — the guard absorbed a 2×
redundancy as latency and spend, instead of the redundancy being fixed.

**Scope: measure it, change nothing.** Same discipline as TE1 — before optimising something, make it
observable. Do **not** implement caching or deduplication. That is §3b, and it is reserved.

**Shape.** In `crates/executor`, count how many tool invocations within a single run are byte-exact
repeats of an earlier one (same tool, same arguments), and record it — on the run's report, in a
`tracing` field at `info`, or both. Then report what it says about real runs.

### Acceptance

- [ ] A run that repeats an identical call reports a repeat count > 0; a run that does not reports 0.
- [ ] Matching is **exact**, not fuzzy. A test shows two near-but-not-equal argument sets counted as
      distinct. (`ARG_SIMILARITY_THRESHOLD` exists for *near-duplicate detection* in the doom-loop
      guard — different mechanism, do not reuse it.)
- [ ] Visible at `info`. The box runs at `info`, and a `debug`-level counter is unobservable in
      production — we learned that the hard way on TE1.
- [ ] **Numbers in the PR**, from a real journal or a real run.
- [ ] **Execution behaviour does not change.** A test asserts a repeated call is still *made*.
- [ ] Workspace green.

---

## 3b. Stop paying for redundant tool calls — **reserved, do not implement**

Reusing a tool result is a **correctness** change wearing a token-optimisation costume. A read is
safe to reuse; a write is not, and "call it again" is sometimes the entire point — polling, or a
retry after failure. Getting it wrong turns a token saving into a silent double-execution bug in the
write path, which is a bad trade at any price.

Kept in-house deliberately. It needs §3a's measurements first regardless.

---

## Conventions

One branch per deliverable, one PR. State the R1 mutation **and its actual output**. Where an
acceptance item asks for a number, the number goes in the PR body — not a promise to measure later.

If you find a deliverable is already done, or the spec is wrong about the code: **say so and stop.**
That is a useful result, not a failure. It has already happened once on this page.
