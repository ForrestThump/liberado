---
kind: plan
status: implemented
authority: advisory
domain: process
canonical_for: parallel-deliverables-r1
open_items: false
---

> **Archived.** This plan is not current truth. Open work lives in [backlog.md](../backlog.md) and [roadmap.md](../../roadmap.md). See [doc-authority.md](../../spec/reference/doc-authority.md).

# Five parallel deliverables

**Written** 2026-08-02, for execution by a separate agent against `main`.
**Shape**: five independent PRs, non-overlapping file sets, each mergeable alone and in any order.

Read [`failure-modes.md`](../../spec/architecture/failure-modes.md) first. Two rules from it govern every
item here:

- **§1 — a check that cannot fail is not a check.** Before landing a test, break the thing it covers
  and watch it fail. Put the evidence in the PR.
- **§6 — two things that should agree, and nothing checks that they do.** Several items below exist
  because exactly that happened.

One rule of my own, learned repeatedly today: **assert the wiring, not just the unit.** A pure
function is easy to test and being *called* is not a property of it. Deleting a call site must fail
something.

## Non-overlap

| # | Deliverable | Owns |
|---|---|---|
| 1 | Token cost accounting | `crates/config-loader` (models table), new `crates/cost` |
| 2 | Per-conversation compaction trigger | `crates/main-agent` (compaction), `crates/server/src/state.rs` |
| 3 | TUI: stop, per-conversation model, reattach | `crates/tui`, `crates/liberado-commands` |
| 4 | Graceful shutdown for in-flight turns | `crates/server/src/lib.rs`, `crates/daemon` |
| 5 | Tier 3: durable-turn path | `crates/conformance` |

`crates/server/src/api/chat.rs` is touched by **nobody**. If an item seems to need it, stop and say
so in the PR rather than reaching in — that file is the seam three of these depend on.

---

## 1. Token cost accounting (D1 + D2)

Full context: [`token-cost-accounting-plan.md`](../token-cost-accounting-plan.md). Build **D1 and D2
only**. D3 (pre-flight estimate) and D4 (surfacing) are explicitly out.

**Why it is first.** Every performance and design argument in this project is currently a guess,
including an open one this week: whether returning research findings inline costs materially more
than returning a status line.

**Do not re-instrument.** `crates/provider/src/latency.rs` already records every inference call —
prompt/completion/total tokens, `cached_prompt_tokens`, model, role, correlation — to
`<data>/latency/events.jsonl`. There are ~1,300 real records on the deployed box. This work reads
that file; it does not add fields to it and does not create a second journal.

**D1 — prices in config.** `[[models]]` in `topology.toml` gains optional per-million rates:
`input`, `output`, `cached_input`. Optional is load-bearing: an unpriced model must yield "tokens
known, cost unknown", never a default rate.

**D2 — a cost query.** New `crates/cost` binary (`role = "tooling"`, copy `crates/eval`'s shape),
reading the journal and reporting:

- cost per **conversation**, rolled up to include the subagent work the turn caused. **This is not a
  group-by**, and getting it wrong is the single most likely way this ships understating the
  expensive path. A face turn's records carry the *chat id*; its subagent's records carry a
  *different* correlation (`chat-delegate-<ulid>`). The link lives in the dispatch journal:
  `<data>/dispatches/<correlation_id>.jsonl`, whose first line carries `parent_conversation`.
  Verified 2026-08-02 — one delegating turn produced 3 face-role records under the chat id and its
  subagent's records under the dispatch id, with 541 dispatch-correlated records in the journal
  overall;
- cost per **role**;
- prompt-token growth per turn within a conversation;
- **cache hit rate** (`cached_prompt_tokens / prompt_tokens`).

**Acceptance**

- [ ] A model with no price entry reports its tokens and a `null` cost. A test asserts an unpriced
      model never contributes 0.0 to a total — it appears on a separate "unpriced" line.
- [ ] Rolling up a conversation whose turn dispatched a subagent includes the subagent's calls.
      Fixture must use a *different* correlation for the child plus a dispatch-journal entry naming
      `parent_conversation` — a fixture where the ids already match would pass a naive group-by and
      prove nothing.
- [ ] A subagent's cost is attributed to its parent conversation and NOT double-counted when the
      dispatch is also queried directly.
- [ ] Prices are applied at read time. A test changes a rate and re-queries the *same* fixture,
      showing a different total — the journal must not bake money in.
- [ ] Runs against the real journal on the box and prints a per-conversation table. Paste it in the PR.
- [ ] `cargo test --workspace`, `cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings`, `cargo fmt --check` all clean.

**Second landmine.** Streaming calls sometimes report no usage at all, and some providers omit
`cached_prompt_tokens` entirely. Absent must be distinguishable from zero everywhere — a reported
zero means "caching available, not working"; absent means "the backend said nothing".

---

## 2. Per-conversation compaction trigger

**The bug.** `resync_compaction_trigger_for_face_model` (`server/src/state.rs:88`) computes one
`trigger_tokens` from the *daemon-wide* face model and calls
`ChatSessions::set_compaction_trigger_tokens` — a single shared number. Conversations now choose
their own model (landed 2026-08-01), so a chat on a 64k-context model and one on a 200k model
compact at the same threshold. One of them is always wrong.

This is a §6 failure: the model a conversation runs on and the threshold it compacts at are two
things that must agree, and nothing checks that they do. The roadmap flags it under CH4 as
"dependent re-resolve".

**Shape.** The trigger becomes a function of the conversation's resolved model, evaluated per turn,
the way `turn_settings` already resolves the model itself. `Config::resolve_trigger_tokens` already
takes a model and the models table — the resolution logic exists; only its *timing and scope* are
wrong.

**Acceptance**

- [ ] Two conversations on models with different context windows compact at different points. Test
      drives both through a real `ChatSessions` with a durable store.
- [ ] A conversation with no model of its own uses the daemon default's trigger — unchanged
      behaviour for every conversation that predates per-conversation models.
- [ ] Changing the daemon-wide model no longer retunes a conversation that has its own. This is the
      assertion that would have caught the bug.
- [ ] Removing the per-conversation resolution fails a test (§1 evidence in the PR).
- [ ] Workspace green as above.

**Out of scope.** CH3.1's viewport rearchitecture. This makes the existing trigger correct; it does
not redesign compaction.

---

## 3. TUI: stop, per-conversation model, reattach

**This one fixes a regression, and it is mine.** Durable chat turns (2026-08-02) detached a turn's
lifetime from its HTTP connection. Before that, closing the SSE stream cancelled the turn — the TUI's
`stop_stream`/`cancel_stream` (`tui/src/app.rs:1042`) relied on exactly that. **They no longer stop
anything.** Ctrl+S and Esc now mean "stop showing me", while the turn runs on and bills.

The WebUI got `POST /api/conversations/{id}/cancel` wired at the same time. The TUI did not.

Three gaps, same surface:

1. **Stop must cancel.** `stop_stream` calls the cancel endpoint, not just `end_stream`. Note the
   daemon's contract: a cancelled turn persists nothing, so the existing "keep partial response"
   wording in the TUI's help (`app.rs:978`) is now a lie and must change too.
2. **`/model` is daemon-wide in the TUI** (`tui/src/api.rs:117`) while the WebUI scopes to the open
   conversation. Same command, two meanings, depending on where you type it. Send the conversation
   id when one is open.
3. **No reattach.** `GET /api/conversations/{id}/attach` replays a running turn and continues live;
   `GET /api/conversations/{id}` reports `turn_running` and `turn_unanswered`. The TUI can neither
   rejoin a turn after a restart nor say that one died.

**Acceptance**

- [ ] Stop issues the cancel request; a test asserts the effect is emitted (the TUI's `Effect`
      pattern makes this assertable without a daemon).
- [ ] Help text no longer promises a partial response is kept.
- [ ] `/model` with an open conversation sends `conversation`; without one, it does not.
- [ ] Opening a conversation with `turn_running: true` attaches; with `turn_unanswered: true` the
      TUI says the turn died rather than showing silence.
- [ ] Workspace green.

**Landmine.** The TUI decodes SSE with `chat_client_contract::SessionEvent::from_sse_data`, shared
with the WebUI. Do not fork it. The attach stream emits the same vocabulary, replayed first.

---

## 4. Graceful shutdown for in-flight turns

**Why.** Every deploy recreates the container, and detached turns die with the process. Today they
are at least *visible* (`turn_unanswered`), but the work is still lost — and we deploy often. On
2026-08-02 a turn ran 205 seconds unwatched; a restart at second 200 would have thrown all of it away.

**Shape.** On SIGTERM: stop accepting new turns, give in-flight ones a bounded grace period to
finish and persist, then exit. Whatever has not finished is left in the state `turn_unanswered`
already describes correctly — so the fallback needs no new concept.

Docker's default stop timeout is 10s, which is likely too short; the compose service may need
`stop_grace_period`. Say what you chose and why.

**Acceptance**

- [ ] A turn in flight at shutdown completes and persists, given time within the grace period.
- [ ] A turn that exceeds the grace period does not block shutdown indefinitely, and afterwards
      reads as `turn_unanswered` — not as running.
- [ ] New turns are refused once shutdown starts, with a distinguishable response rather than a
      generic failure.
- [ ] Removing the grace period fails a test.
- [ ] Workspace green.

**Out of scope.** Resuming a turn across a restart. Inference is not replayable from where it
stopped; this is about finishing what can finish, not reviving what cannot.

---

## 5. Tier 3: a durable-turn path

**Why.** Durable turns, attach, and cancel have unit tests and one manual live check. They have no
Tier 3 coverage, and Tier 3 exists precisely because in-process tests could not see the defects that
mattered. This is the newest, least-exercised surface in the daemon.

Current operation: [`live-conformance.md`](../../impl/live-conformance.md).
Every rule there still applies — background sessions only, conformance zone only, touch only what
the run created.

**New path P6 — a turn outlives its connection.** Against the deployed daemon:

1. start a background chat turn, drop the connection early;
2. assert `turn_running` stays true with nobody attached;
3. attach, and assert replay arrives before live events;
4. assert the reply is **on disk** when it finishes — the ground truth, not an event saying so;
5. separately: start a turn, cancel it, assert it stops **and persists nothing**.

**Acceptance**

- [ ] P6 passes against the live daemon; paste the run.
- [ ] P6 appears in `forced_fail_matrix.rs` with the daemon mocked into each broken condition —
      a turn that dies on disconnect, an attach that replays nothing, a cancel that does not cancel.
- [ ] The rollback assertion is real: cancelling must leave the transcript with the question and no
      reply, and the test must fail if a partial answer is persisted.
- [ ] Workspace green.

**Landmine.** The cancel assertion is the one most likely to be written vacuously. "It stopped" is
observable from the outside; "it persisted nothing" requires reading the transcript afterwards, and
that is the half that guards the rollback guarantee.

---

## Conventions

- Branch per deliverable off `main`, PR into `main`, no stacking.
- Commit messages explain **why**, not what — match the existing history's voice.
- Do not commit `docs/future-work/session-profiles-next-actions.md` (gitignored working notes).
- CI runs `cargo fmt --check` first; it has failed a branch on formatting alone before.
- `deploy/homelab/config/*` changes are part of a deliverable when needed, and part of its review.
