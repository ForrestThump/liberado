# Round 3 — three deliverables, in priority order

Written 2026-08-02, after all five round-2 PRs (#33–#37) landed.

**Budget note:** roughly 13% of the weekly allowance remains. These are ordered so that stopping
after **one** still lands the most valuable thing, and stopping after **two** still lands the
measurement work the token-economics push depends on. Do not start §3 before §1 and §2 are open.

These are deliberately larger than round 2's. Small, well-understood changes are cheaper to
implement directly than to spec, review and repair — the scoping-plus-review overhead is only worth
paying on work with real design content in it.

---

## What round 2 taught us

Every one of the five branches was competent, and every one shipped a test that claimed more than it
checked. The mechanism changed each time; the shape did not.

| PR | What the tests missed |
|---|---|
| #34 | `token_usage_total` was a lifetime sum, while both consumers render it *against* `context_window` as a percentage. The fixture had one event, so it could not distinguish a running total from context occupancy. Separately, turn grouping broke on the child→parent join — all four turn tests were single-correlation. |
| #35 | An entire acceptance item (the unanswered-turn note) shipped with **no test**. Deleting the feature left all five tests green. |
| #36 | `p7_fails_when_turn_running_after_restart` did not test what it was named for: its fixture was *also* a lost turn, so it failed through the other clause. The zombie guard could be deleted undetected. |
| #37 | `work_start_inventory_is_documented_in_module_docs` **could not fail** — it searched its own source via `include_str!`, and its needle array contained every string it searched for. Separately, a durability gap was masked because the test polled for 2s after the drain, time production does not have. |

**The common root:** tests were aimed at the mechanism the implementer had in mind, not at the
guarantee a user experiences. Three new rules follow, on top of R1–R5 from
[round 2](parallel-deliverables-2026-08-round-2.md), which all still apply.

- **R6 — assert the observable end state, not the internal signal.** `DrainOutcome.parked_goals` is
  what the code *attempted*; the status on disk is what a human finds. `saw_token` is what streamed;
  the transcript is what persisted. When those two can disagree, the test must read the second one.
- **R7 — a fixture that cannot distinguish two implementations is not a test.** Before writing the
  assertion, write down the wrong implementation you are excluding, and check your fixture would
  actually fail it. One-event fixtures and single-correlation fixtures failed this repeatedly.
- **R8 — no test may read a file it is also contained in.** `include_str!` on your own module is how
  a check becomes unfalsifiable. If you are asserting on documentation, scope the read to the doc
  lines and say so.

**And the round-2 habit worth keeping:** three of the five branches stated an R1 mutation in a doc
comment. Two of those statements were wrong when actually run. Run it, paste it.

---

## Non-overlap

| # | Deliverable | Owns |
|---|---|---|
| 1 | Delegated findings reach the face | `crates/orchestrator` (**`delivery_directive` + the `Summarize` path only**), `crates/dispatch-pack`, `crates/main-agent/src/face.rs` |
| 2 | Make subagent vs direct execution distinguishable | `crates/provider/src/latency.rs`, `crates/cost`, `crates/bootstrap`, `crates/orchestrator` (**the role-scope call sites only**) |
| 3 | The executor's accumulation term | `crates/executor` |

**1 and 2 both touch `crates/orchestrator/src/lib.rs`**, which is why ownership is stated per
*function*, not per file. §1 owns the delivery-directive/report-contract region; §2 owns wherever a
role scope gets opened around dispatch execution. If either finds it needs the other's region, stop
and say so in the PR rather than reaching across — that call went fine twice in round 2.

`crates/provider/src/latency.rs` was owned by nobody in round 2 on purpose. §2 owns it now, and it is
a **contract with a reader**: `crates/cost/tests/journal_shape.rs` guards the writer/reader shape and
must be updated in the same PR.

---

## 1. Delegated findings must reach the face agent

**Why this is first.** It is the most serious open correctness bug in the tree: the system returns
confident, well-structured answers whose *provenance is false*. It is fully diagnosed —
[`delegated-work-is-discarded-at-the-seam.md`](delegated-work-is-discarded-at-the-seam.md) has the
root cause, the reproduction (twice, on different models), and the fix shape. Nothing about it is
speculative, and it is the one item here a user would notice.

**Root cause, already found.** `submit_report`'s schema asks for a summary that is
`"High-signal, human-readable, short."` and an `artifacts` field for *vault paths*. The model
complies. `orchestrator::delivery_directive` — which tells a subagent its summary **is** the
document — is appended only when `delivery_target` yields a path, and a chat `delegate` is
`Delivery::Summarize`, which returns early. **The good contract exists and never runs on the path
that needs it most.**

**The context-cost objection is now measured away** and does not need re-litigating: face context is
4.5% of spend, carried context is 76.4% cached, and 136 of 185 conversations are a single turn. See
[`token-economics-findings-2026-08.md`](token-economics-findings-2026-08.md).

**Shape.** `Delivery::Summarize` needs its own directive. The destination differs (a conversation,
not a file) but the contract is identical: *the summary is the material, not a status*. The coupling
to break is that the directive keys on **having a file path** when what matters is **whether the
summary is the deliverable**. `is_read_only_dispatch` already distinguishes research from action a
few lines away.

**Acceptance**

- [ ] A chat `delegate` of a research goal returns findings, not a status line. Assert on the
      **content the face agent receives** — R6: the tool result is the observable, not that a
      directive function was called.
- [ ] The directive is chosen by *whether the summary is the deliverable*, not by the presence of a
      path. A test covers `Summarize` **and** `Vault` and shows the vault path's behaviour unchanged.
- [ ] R7: state the wrong implementation you are excluding. "Appends a directive for every dispatch"
      is one; an action dispatch should not be told to write an essay.
- [ ] **Nothing on the must-not-regress list moves** — `append_note` still authors as
      `Named("goal-session")`, and the tool node still carries no model stamp. Both are load-bearing
      for model derivation and `last_turn_unanswered`; the doc explains why. A test for each.
- [ ] Measure the change: run one real delegation before and after and put the tool-result size and
      the face's follow-up prompt size in the PR. The prediction from the findings doc is that face
      context rises by single-digit thousands of tokens and that this is worth it. Confirm or refute
      with numbers.
- [ ] Workspace green.

**Landmine.** Do not route `delegate` through `append_note`/direct delivery. That was considered and
**rejected by the owner** — the face agent holds conversation context the subagent does not and must
decide how much to relay. This deliverable makes that choice *possible*; it does not take it away.

---

## 2. Make subagent and direct execution distinguishable in the journal

**Why second.** 92.8% of all tokens are journaled as `orchestrator`, and that bucket merges
delegated subagent runs with `ExecuteDirect`. The question the dispatcher exists to answer well — *is
delegating cheaper than doing it directly?* — is unanswerable today, and every token decision after
this one is guesswork until it is fixed. This is the measurement the token-economics push runs on.

**This is not an enum addition.** The role is bound **at provider construction**
([`bootstrap/src/lib.rs:203`](../../crates/bootstrap/src/lib.rs#L203)):

```rust
subagent: Some(role_provider(ModelRole::Subagent, AgentRole::Orchestrator)),
```

One provider instance serves both dispatch paths, so a new `AgentRole::Subagent` variant has nowhere
to be set from. Nor can the dispatch journal answer it: that is written by the face's `delegate`
(`crates/main-agent/src/dispatch_journal.rs`), so it covers chat delegations only — and by
correlation prefix, chat is 94 of 191 orchestrator runs. Cron (22) and vault reactions (58) never
appear in it.

**Three options. Pick one and justify it in the PR:**

1. **A task-local role scope**, mirroring `latency::with_correlation`. The orchestrator opens it per
   dispatch. Same landmine as correlation: `tokio::spawn`ed children do **not** inherit, and parallel
   subagents are spawned.
2. **A second provider instance** with `AgentRole::Subagent`, chosen by the orchestrator per dispatch
   action. No task-local subtlety; more plumbing, and two providers that must stay configured alike.
3. **A separate field** (e.g. `dispatch_kind`) rather than overloading `role`. Keeps `role` meaning
   "which agent" and adds "under what decision", which are genuinely different questions. Costs a
   journal field.

**Acceptance**

- [ ] A delegated subagent call and an `ExecuteDirect` call are distinguishable in
      `<data>/latency/events.jsonl`, and `liberado-cost` reports them separately.
- [ ] **Cron- and vault-triggered runs are covered too** — not just chat delegations. A test proves
      it for at least one non-chat trigger. This is the case option 3's cheap version misses.
- [ ] `crates/cost/tests/journal_shape.rs` updated in this PR, and a reader that predates the change
      still parses new records (the reader defaults unknown fields; prove it).
- [ ] R7: the fixture must contain **both** kinds. A fixture of only subagent calls would pass an
      implementation that labels everything `subagent`.
- [ ] Re-run `liberado-cost` against a real journal and put the new split in the PR. The open
      question is what share of the 92.8% is delegation versus direct execution.
- [ ] Workspace green.

**Landmine.** Do not change the meaning of existing `role` values. `face`, `dispatcher` and
`orchestrator` records already on disk must keep parsing and keep meaning what they meant, or every
historical comparison silently shifts.

---

## 3. The executor's accumulation term

**Why third, and why it is still worth doing.** Of the orchestrator's 13.95M prompt tokens, **37.4%
is accumulation inside the agent loop** — tool results piling up hop over hop. That is the quadratic
term, it is the one delegation does *not* help with, and it overtakes base re-sending at around 9
hops. There is also a documented, concrete instance of waste in it: a live `evening-debrief` run
called `liberado-caldav-mcp:list_events` **four times for two dates** before the doom-loop guard
fired. The run *succeeded*, which is the problem — the guard is absorbing routine redundancy as
latency and spend rather than the redundancy being fixed.

**Shape.** Two separable pieces; do the first, and the second only if budget allows.

1. **Do not pay twice for the same call.** An identical tool invocation (same tool, same arguments)
   already made in this run has a known result. Return it without a round trip, and without a second
   copy in the transcript.
2. **Bound what a result contributes to context.** `CompactionConfig::tool_result_max_chars` already
   truncates tool results for the *summarizer*; the executor's own loop has no equivalent.

**Acceptance**

- [ ] A run that would repeat an identical tool call makes the underlying call once. Assert the
      **invocation count at the runtime boundary** (R3/R6), not that a cache struct was populated.
- [ ] The doom-loop guard still fires for genuine pathology. R7: the wrong implementation to exclude
      is one that suppresses repeats so effectively the guard never sees a real loop. A test drives
      an actual doom loop and shows the guard still catches it.
- [ ] Identical-call detection is **exact**, not fuzzy. `ARG_SIMILARITY_THRESHOLD` exists for
      near-duplicate *detection*; reusing a result requires the arguments to be the same, and a test
      shows two near-but-not-equal calls both execute.
- [ ] Measure it: replay or re-run a multi-hop dispatch and report tokens before/after in the PR.
- [ ] Workspace green.

**Landmine.** A tool result is not always idempotent to reuse. A read is; a write is not, and
"call it again" may be the point (polling, retry after a failure). Scope reuse to calls the catalog
marks read-only, or state explicitly why a broader rule is safe. Getting this wrong turns a token
optimisation into a correctness bug, which is a bad trade at any price.

---

## Conventions

Unchanged from round 2: one branch per deliverable, `cargo fmt --check`, `cargo test --workspace`,
and `cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings` all green before
the PR. State the R1 mutation **and its actual output**. Where an acceptance item asks for a number,
the number belongs in the PR body, not in a promise to measure later.
