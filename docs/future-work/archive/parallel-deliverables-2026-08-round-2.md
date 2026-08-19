---
kind: plan
status: implemented
authority: advisory
domain: process
canonical_for: parallel-deliverables-r2
open_items: false
---

> **Archived.** This plan is not current truth. Open work lives in [backlog.md](../backlog.md) and [roadmap.md](../../roadmap.md). See [doc-authority.md](../../spec/reference/doc-authority.md).

# Five parallel deliverables — round 2

**Written** 2026-08-02, for execution by a separate agent against `main`.
**Shape**: five independent PRs, non-overlapping file sets, each mergeable alone and in any order.
**Predecessor**: [`parallel-deliverables-2026-08.md`](parallel-deliverables-2026-08.md) — all five landed
as PRs #28–#32. Read the lessons below before reading the specs; they are the most useful thing here.

Read [`failure-modes.md`](../../spec/architecture/failure-modes.md) first. §1 (*a check that cannot fail
is not a check*) and §6 (*two things that should agree, and nothing checks that they do*) still govern
every item.

---

## What round 1 taught us

All five PRs were structurally sound and merged. Every one of them also shipped **at least one test
that claimed more than it checked**, and in four cases the claim was about the deliverable's headline
feature. This is not a complaint — it is a calibration. The same failure shape recurred five times,
which makes it predictable and therefore preventable.

| PR | The test said | What it actually checked |
|---|---|---|
| #28 | the unpriced section is rendered | `text.contains("unpriced")` — which matched a **column header**, so it passed with the section empty |
| #29 | "if `maybe_compact` were changed to always read `default`, this fails" | only the **query API**; the named mutation left it green |
| #30 | reattach works | that an `Effect` value was **emitted**. Gutting the effect body left all **278** tests passing |
| #31 | attach replayed real turn content | `data.contains("token")` — any `progress` or `tool_started` frame whose free text says "token". P6 **passed** against an attach replaying nothing |
| #32 | new turns are refused during shutdown | the **HTTP routes**. Telegram calls `ChatSessions::turn` directly and was never gated |

Three distinct mechanisms, worth naming separately because they need different countermeasures:

**A. Loose matching on a haystack that contains the needle for other reasons.** (#28, #31.) A
substring check against rendered text or a JSON blob will find the word somewhere. Assert on
structured values — `report.unpriced.iter().any(|u| u.model == "…")`, `event_name == "token"` — not on
whether a string appears in a page.

**B. Testing the nearest seam instead of the behaviour.** (#29, #30, #32.) The test reaches for what
is easy to call: the pure function, the emitted enum value, the route. The behaviour lives one layer
further — the compaction path, the network call, the capability. **If the feature crosses a boundary,
the test must cross the same boundary.** An effect that is only ever asserted as a value can be a
no-op in production forever.

**C. A rule written twice, where the tests assert against the copy.** (#29's `peek_turn_model`
duplicating `turn_settings`'s precedence.) The copies had already silently diverged. When production
drifts, the tests follow the copy and stay green.

### Rules for this round

Beyond everything in the previous doc, which still applies:

- **R1 — run the mutation you claim.** If a test's doc comment says "deleting X fails this", *delete X
  and paste the failure in the PR*. If it does not fail, either the test or the sentence is wrong;
  fix whichever is wrong before landing. A test that overstates its reach is worse than a missing one,
  because it tells the next reader the case is already covered.
- **R2 — no substring assertions on rendered output or serialized blobs.** Match on parsed structure.
  If you must assert on text, assert on a string that cannot occur for another reason, and say in a
  comment why it cannot.
- **R3 — cross the boundary the feature crosses.** Emitting an effect is not performing it; a route
  guard is not a capability guard; a query API is not the code path that runs. Name the boundary in
  the test's doc comment so the next reader knows which side is covered.
- **R4 — if you find yourself writing a second copy of a resolution rule, stop.** Extract it, and
  parameterise the one thing that genuinely differs.
- **R5 — state your scope honestly in the doc comment.** "This exercises the query, not the turn" is a
  perfectly good test. Pretending otherwise is the defect.

**Ordinary caveat:** none of this means write more tests. Round 1's suites were large. The problem was
aim, not volume.

---

## Non-overlap

| # | Deliverable | Owns |
|---|---|---|
| 1 | Correlation coverage — repair the cost instrument | `crates/main-agent/src/sessions.rs`, `crates/server/src/api/chat.rs` |
| 2 | Turn-aware cost + surface the total | `crates/cost`, `crates/server/src/api/status.rs` |
| 3 | Telegram surface parity | `crates/server/src/telegram.rs` |
| 4 | Goal sessions get a shutdown grace | `crates/server/src/shutdown.rs`, `crates/server/src/lib.rs`, `crates/session` |
| 5 | Tier 3 P7 — restart survival | `crates/conformance` |

`crates/provider/src/latency.rs` is touched by **nobody**. Round 1 established that the journal's
shape is a contract with a reader (`crates/cost/tests/journal_shape.rs` guards it); changing it needs
its own PR, not a side effect of one of these.

1 and 3 both end up caring about Telegram, from opposite sides — 1 owns *how a turn is recorded*
inside `ChatSessions`, 3 owns *what the bridge does*. If 3 finds it needs to edit `sessions.rs`, or 1
finds it needs to edit `telegram.rs`, stop and say so in the PR rather than reaching across.

---

## 1. Correlation coverage — repair the cost instrument

**Why it is first.** `liberado-cost` landed in #28 and immediately reported that **8% of inference
calls cannot be attributed to anything**: 104 of 1,326 records on the box carry `correlation: "-"`,
which `latency::current_correlation()` returns outside any `with_correlation` scope. At illustrative
rates that was $0.157 of $3.07. Worse than the money: the bucket contains a `face`-role conversation
growing 930 → 29,904 prompt tokens turn over turn — a real chat whose entire cost is invisible to the
per-conversation rollup that #28 exists to provide.

**Root cause, already found — do not re-diagnose it.** `ChatSessions::spawn_turn` wraps its work in
`with_correlation(session)` ([`sessions.rs:1176`](../../../crates/main-agent/src/sessions.rs#L1176)).
`ChatSessions::turn` ([`sessions.rs:723`](../../../crates/main-agent/src/sessions.rs#L723)) — the
non-streaming path — does not. Its two production callers are:

- `POST /api/chat` ([`api/chat.rs:462`](../../../crates/server/src/api/chat.rs#L462))
- the Telegram bridge ([`telegram.rs:57`](../../../crates/server/src/telegram.rs#L57))

So **every Telegram turn and every non-streaming HTTP turn is unattributed**. That is the bulk of it.

**Do not assume that is all of it.** `with_correlation` rides a tokio task-local, and
`latency.rs`'s own doc warns that `tokio::spawn`ed children do **not** inherit — `orchestrator`
re-wraps at its spawn point for exactly this reason. There are only four wrap sites in the whole
workspace (`git grep with_correlation`); audit what runs inference outside them. Compaction
summarisation and default-title seeding are both worth checking specifically: they are model calls
made inside a turn, and if they inherit correctly that is a fact worth a test rather than an
assumption.

**Acceptance**

- [ ] A turn started through `ChatSessions::turn` records the conversation id, not `-`. Assert it with
      a recording `LatencyRecorder` double capturing `LatencyEvent.correlation` — **R3**: the wrap is
      inside `ChatSessions`, so the test must observe what the recorder saw, not that a function was
      called.
- [ ] Removing a wrap fails a test, and the PR shows the failure (**R1**).
- [ ] Compaction summarisation and title seeding are covered one way or the other: either a test shows
      they inherit the turn's correlation, or the PR states they deliberately do not and why.
- [ ] `-` still means "genuinely uncorrelated" — nothing invents a plausible id to make the bucket
      look smaller. A test asserts a call made outside any scope still records `-`.
- [ ] Re-run `liberado-cost` against the box after deploy and paste the before/after `-` line. If a
      residue remains, say what it is; a smaller unexplained bucket is not the goal, an explained one
      is.
- [ ] `cargo test --workspace`, `cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings`, `cargo fmt --check` clean.

**Landmine.** Do not fix this by defaulting the correlation inside `MeteredProvider` or at the
provider seam. The provider does not know which conversation it is serving, and a fabricated id would
turn a visible gap into an invisible wrong answer — strictly worse, and unfalsifiable afterwards.

---

## 2. Turn-aware cost, and the total nobody can see

**Why.** #28 delivered D1 and D2. Two things stop it from answering the design questions it was built
for.

**Gap A — a "turn" in the growth table is one inference call.** A face turn with three tool-calling
hops produces three rows, so the deltas mix intra-turn hops with turn-over-turn growth. That is
honestly documented in `TurnGrowth`'s doc comment, and it is also the exact number the
inline-findings question needs. Turn boundaries **are** derivable from data already in the journal:
`LatencyEvent.tool_calls == 0` means the model stopped asking for tools, so that call ends a turn.
Treat it as the heuristic it is — say so in the type's docs, and handle `finish == "error"` (a turn
can end by failing).

**Gap B — `/api/status.token_usage_total` is still `null`**, a stub that has never been filled. The
cost crate can answer it now.

**Shape.** `liberado-cost` gains turn grouping over the existing records. `liberado-server` fills
`token_usage_total` by reading the journal through the cost crate. No new journal, no new field on
`LatencyEvent` — D3 (pre-flight estimate) stays out of scope, because it needs a writer change and
that is a different PR by the ownership rule above.

**Acceptance**

- [ ] A conversation whose turn made three tool-calling hops reports **one** turn row, not three.
      Fixture must contain a multi-hop turn; a fixture where every call has `tool_calls == 0` would
      pass a per-call implementation and prove nothing (**R1**, and the same trap as #28's rollup
      fixture).
- [ ] Per-turn prompt growth is turn-over-turn. A test asserts the delta between turn 1 and turn 2
      ignores the hops inside turn 1.
- [ ] A turn that ends in `finish == "error"` still closes a turn rather than merging into the next.
- [ ] `token_usage_total` is populated and matches the journal. Assert the parsed field, not a
      substring of the JSON body (**R2**).
- [ ] Unpriced and absent-usage handling still hold at turn granularity — an unpriced call inside a
      turn does not make the turn's cost `0.0`.
- [ ] **Answer the open question with a number.** `delegated-work-is-discarded-at-the-seam.md` leaves
      the context cost of `relay_directive` unresolved. Now measurable: what do the turns *after* a
      delegating turn cost, versus after a non-delegating one, and what is the cache hit rate on them?
      Put the figures in the PR. This is the whole reason D1/D2 were built first.
- [ ] Workspace green.

**Landmine.** Do not add a second journal, and do not write money into the existing one. Pricing stays
a read-time operation — that decision is load-bearing for historical comparison and is already
documented in [`token-cost-accounting-plan.md`](../token-cost-accounting-plan.md).

---

## 3. Telegram surface parity

**Why.** Telegram is the surface actually dogfooded daily, and it is now the least correct one. The
TUI got stop/scope/reattach in #30; Telegram got nothing.

**The headline bug is that `/model` lies, and round 1 is what made it a lie.**
[`telegram.rs:153`](../../../crates/server/src/telegram.rs#L153) calls `provider.set_model(...)` — the
**process-wide** default — and replies `"Model switched: X → Y"`. But since per-conversation models
landed, `resolve_turn_model`'s precedence is *pending pick → profile model → what this conversation
last ran on*. The sticky Telegram conversation has history, so it resolves via `model_last_used` and
**keeps running on its previous model**. The command reports success, changes a global, and does not
affect the conversation the human typed it in. It also silently retunes every other chat that has no
model of its own.

Three gaps, one surface:

1. **`/model` must scope to the sticky conversation** — `ChatSessions::select_model(conversation, …)`,
   the same call the WebUI and (since #30) the TUI make. The reply must state the scope truthfully.
2. **No way to stop a turn.** Durable turns outlive their caller; on Telegram there is no cancel at
   all. Add one (`/stop`, or whatever fits `liberado-commands`' existing vocabulary) hitting the same
   cancel path, and say plainly that a cancelled turn keeps nothing — the lie #30 had to remove from
   the TUI's help text must not be reintroduced here.
3. **No lifecycle awareness.** If a turn is already running for the sticky conversation, a new message
   should say so rather than queueing behind the session lock with no feedback; if the last turn ended
   unanswered (`turn_unanswered`), say that instead of silence.

**Acceptance**

- [ ] `/model <id>` on Telegram sets the model for the sticky conversation, and **the next turn
      actually runs on it** — assert the model stamped on the log, not the reply text (**R3**: the
      reply is the easy seam, the stamp is the behaviour).
- [ ] A test asserts `/model` on Telegram no longer changes the process-wide default while a sticky
      conversation exists. This is the assertion that would have caught the current bug.
- [ ] With no sticky conversation yet, behaviour is deliberate and stated — either create one, or fall
      back to daemon-wide. Either is defensible; silently doing the old thing is not.
- [ ] Cancel stops an in-flight turn and the reply does not promise a partial answer was kept.
- [ ] A message arriving while a turn runs gets a distinguishable response.
- [ ] Workspace green.

**Landmine.** `TelegramChatBridge` shares its sticky session id with the cron-delivery notifier — that
shared id is the whole mechanism behind "replying to a brief has the brief in context"
(`docs/ideas/cron-delivery-timing-idea.md`). Do not change how the sticky id is created or stored
while fixing what `/model` does to it.

---

## 4. Goal sessions get a shutdown grace too

**Why.** #32 drains in-flight **chat turns** on SIGTERM. `in_flight_count` reads
`ChatSessions::running` and nothing else, so a goal session mid-run — the long ones, the coding and
research work — still dies with the process, with no grace and no notice. And `POST /api/goals` is not
behind the drain gate, so a new goal session can start *during* shutdown and be killed seconds later.

That second half is the same defect shape #32's review found in the Telegram bridge: **the gate covers
the routes someone remembered, not the capability**. Enumerate every way work can start, and gate the
capability.

**Shape.** Extend the drain to goal sessions. `GoalSessionHub` already has `park()` — parking a
session is very likely better than aborting it, because a parked session can be resumed after the
restart while an aborted one cannot. Decide, and justify the decision in the PR; do not just mirror
the chat behaviour because it is there.

**Acceptance**

- [ ] A goal session in flight at shutdown gets the grace period, and finishes or parks — not killed
      outright.
- [ ] `POST /api/goals` (and any other work-starting route) refuses with the same `shutting_down` kind
      once draining. A test drives the **real router with the middleware attached**, as
      `shutdown.rs`'s existing `turn_start_router` helper does — asserting the gate flag alone would
      not catch a route that was never added to the layer (**R3**).
- [ ] The PR lists every way work can start and states, for each, whether it is gated and why. Chat
      HTTP, chat Telegram, goals HTTP, hooks, cron, vault reactions. This list is the deliverable as
      much as the code is.
- [ ] Removing goal-session drain fails a test (**R1**, with the failure pasted).
- [ ] Post-drain state is coherent and asserted: a session that did not finish reads as something a
      human can act on, not as permanently `running`.
- [ ] Workspace green.

**Out of scope.** Resuming inference mid-call across a restart. Parking is a session-level checkpoint,
not a way to continue a model call that was interrupted.

---

## 5. Tier 3 P7 — restart survival

**Why.** #32's drain has unit tests against a real `ChatSessions` with a durable store, and **nothing
has ever sent SIGTERM to the deployed daemon mid-turn**. Tier 3 exists precisely because in-process
tests could not see the defects that mattered, and this is the newest unexercised guarantee in the
system. P6 (durable turn outlives its connection) landed in #31 and is the template.

Current operation: [`live-conformance.md`](../../impl/live-conformance.md).

**New path P7 — a turn survives, or is honestly reported, across a restart.**

1. start a background chat turn;
2. trigger the restart;
3. assert the daemon refused new turns during the drain window (`503` + `shutting_down`);
4. after it comes back, assert the conversation reads **either** with the assistant reply persisted
   (finished within grace) **or** as `turn_unanswered` — and **never** as `turn_running`, which after a
   restart is a claim about a process that no longer exists.

**This path restarts the daemon, which no other path does.** Treat that as the hard part:

- It must be **opt-in via config**, off by default, and never in `all_default()`. A conformance run
  that reboots the box by surprise is worse than no coverage.
- The restart command belongs in config, not hard-coded — the box uses
  `docker compose … up -d --force-recreate`; a dev machine does not.
- If the restart hook is not configured, P7 must **skip with a stated reason**, not pass. A skip that
  reports as a pass is the §1 failure in its purest form.

**Also decide P6's and P7's membership in the default set.** P6 is currently registered, non-advisory,
and *not* in `all_default()` — so a plain suite run still does not exercise durable turns. That was
probably right (two long real-inference turns), but it is currently implicit. Make it explicit in
`result.rs` with a comment saying which paths cost real inference and which do not.

**Acceptance**

- [ ] P7 passes against the live daemon; paste the run.
- [ ] P7 appears in `forced_fail_matrix.rs` with the daemon mocked into each broken condition: new
      turns accepted during drain, a conversation still reporting `turn_running` after restart, and a
      turn lost with no `turn_unanswered` to show for it.
- [ ] Unconfigured restart hook produces a **skip with a reason**, and a test asserts a skip is not
      counted as a pass.
- [ ] The default-set decision is written down in code, not left implicit.
- [ ] Workspace green.

**Landmine.** The assertion most likely to be written vacuously is step 4. "The conversation exists
after restart" is trivially true. The property is that its *lifecycle flags are honest* — and the case
that matters is the one where the turn did **not** finish, because that is where a stale
`turn_running` would strand a human waiting for a reply that no process is producing.

---

## Conventions

- Branch per deliverable off `main`, PR into `main`, no stacking.
- Commit messages explain **why**, not what — match the existing history's voice.
- Do not commit `docs/future-work/session-profiles-next-actions.md` (gitignored working notes).
- CI runs `cargo fmt --check` first; it has failed a branch on formatting alone before.
- `deploy/homelab/config/*` changes are part of a deliverable when needed, and part of its review.
- **Every PR body includes its R1 evidence** — the mutation run, and the test output showing the
  failure. If a claimed mutation does not fail, say so and fix it; reviewers will run it anyway.
