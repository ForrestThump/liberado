# One execution engine, and sessions that wait for you

**Status**: landed 2026-07-13/14 (E1–E7).

A **within-run** guided retry ships with E5: a build that fails asks the human and folds their answer
into the next attempt's `prior_feedback` — the same channel the verifier repair loop already uses,
against a workspace that still holds the failed attempt's changes. No checkpoint is needed for that,
because nothing is being *replayed*: the process never died.

What remains deferred is narrower than it first looked — resuming a session **across a daemon
restart** while it was mid-build. E6 parks such a session honestly (`Parked`, `awaiting_input`
preserved) rather than pretending; answering it after a restart does not yet resume the build. That
is the case that needs a workspace checkpoint (E6-c).
**Debt item**: #1 from the 2026-07-13 hygiene audit ([`../../roadmap.md`](../../roadmap.md)).
**Related**: [`../../spec/architecture/sessions.md`](../../spec/architecture/sessions.md) ·
[`agentic-loops.md`](../../spec/architecture/agentic-loops.md) ·
[`channels-and-interactivity.md`](../../spec/architecture/channels-and-interactivity.md) ·
[`verifiers.md`](../../spec/architecture/verifiers.md)

---

## 1. Two problems, and the second is the one that matters

**The stated problem** is that D7 unified how sessions are *stored and displayed* but not how they
are **run**. There are two engines — `GoalSessionHub` + packs, and `Dispatcher` + `Orchestrator` —
and everything on the second one is *recorded, not hosted*: a background session's `domain` is the
fake value `"dispatch"`, joining one is read-only, and authority is described two different ways.

**The problem worth solving** turned up while sketching the first one:

> *"Agent flows where you basically work until the task is complete or I need to make a choice could
> be very useful — almost essential for the coder pack. Crons should generally just run."*

That is right, and it inverts the priority. The interesting capability is **a goal-pursuing session
that pauses for a human decision and waits as long as it takes** — hours, across a working day. A
cron is merely one caller, and mostly it will not use it.

Two things block that today, and neither is the engine split:

1. **The coder pack can only ask during *intake*.** `CodingSessionPack::ask` is reached from the
   clarify rounds and the freeze prompt. Once `backend.run(request)` starts, there is **no seam at
   all** to stop and ask. So "work until done *or until I need a choice*" is precisely what it cannot
   do — it can only guess, or fail.
2. **An awaiting session dies on a daemon restart.** `replay_file` coerces any non-terminal goal
   session to `Failed`, and an awaiting session is `Running`. So a session that waits eight hours for
   an answer is a **lie** unless the daemon happens not to restart — and this daemon restarts
   constantly, because you are actively developing it.

The engine convergence is still worth doing (it is what makes a paused session joinable, answerable
and cancellable *at all*, rather than a read-only recording). But it is the **substrate**, and §5 is
the point.

## 2. The realization the convergence rests on

**The dispatcher + orchestrator pair is already a `DomainPackRunner` in everything but name.**

```
DomainPackRunner::run(session_id, goal, ctx, events, inputs, cancel) -> Result<GoalResult, PackError>

dispatch/orchestrate:  goal -> DispatchDecision -> Disposition
                       Disposition::terminal_summary() -> (TerminalKind, String)   // already exists
```

Every input it needs is already on the session: the goal text is `goal.description`, the authority is
`ctx.grant.capabilities`, the correlation id is `goal.origin.correlation_id`; the catalog and
zone-write-classes are pack-held config. It produces a terminal result.

So this is not "build a third thing". It is **`DispatchPack: DomainPackRunner`** — one new `pack`-role
crate wrapping the dispatcher and orchestrator — after which cron, webhooks and `delegate` start
sessions exactly the way `/spawn` does, and the second engine stops existing.

```
                    ┌───────────────────────────────┐
  /spawn ──────────►│                               │──► CodingSessionPack
  POST /api/goals   │        GoalSessionHub         │──► LifeOpsDemoRunner
  cron ────────────►│  (the ONE execution engine)   │──► DispatchPack ──┐
  webhook ─────────►│                               │                   │
  delegate ────────►│                               │   Dispatcher + Orchestrator
                    └───────────────────────────────┘   (per pool)
```

## 3. Decisions (settled 2026-07-13)

| # | Decision | Why |
|---|---|---|
| **D-a** | **Fix authority first.** `Orchestrator::run` takes per-run capabilities and intersects them **narrow-only** with its own. | `Orchestrator` gates on `self.capabilities`, fixed at `new()`. So a `SessionGrant` *narrower* than its pool — the entire point of a session profile — **cannot be honored**. Shipping the pack first would mean shipping a grant that *looks* enforced and is not. That is the exact class of bug this audit keeps finding (the `AskHuman` gate, the search root, the tests on the wrong store), and it is not one to add on purpose. |
| **D-b** | **Additive `ReactionOutcome::Dispatched { session_id }`; reactions run concurrently.** | Honest about what actually happened (a session started), turns the reactions feed into a *navigation surface* rather than a flat log, and removes a serialization limit nobody chose (today `react()` is awaited inline, one at a time). Additive, per D5. |
| **D-c** | **The idle budget is configurable per profile and may be hours.** A session awaiting a human **notifies out-of-band** when nobody is watching, and the human can answer later. | This is the actual feature (§1). |
| **D-d** | **Crons default to no `AskHuman`.** Interactivity stays a capability, not a mode. | A cron should generally just run. Nothing changes for it unless its profile explicitly grants `AskHuman`. |
| **D-e** | **Delegated sessions run without `AskHuman` in E4.** | `delegate` is synchronous inside a chat turn; routing a mid-turn question back through that turn is a real interaction-design problem, not a detail. Behavior stays identical to today, and it is revisited on its own once background asking works. |

## 4. Slices E1–E4 — the substrate

### E1 — Authority actually binds

`Orchestrator::run(decision, goal, correlation_id, capabilities)`, intersecting narrow-only with
`self.capabilities` (Decision 4: authority never widens). Threads through the guard pipeline and
`RiskGatedToolRuntime` construction.

**Ships**: a session grant that is genuinely enforced on a dispatch execution — so a
`[[session_profiles]]` entry can narrow a cron below its pool, and mean it.
**Proves it**: a profile granting only `Read` runs a dispatch session that is *refused* the write its
pool would have allowed.

### E2 — `liberado-dispatch-pack`

New crate, role `pack`. Deps: `session`, `dispatcher`, `orchestrator`, `common`.

- Holds `HashMap<pool_name, (Dispatcher, Orchestrator)>` — pools stay, because an `Orchestrator` owns
  an `McpRegistry` that is not shareable across instances.
- Reads the pool from `goal.payload["pool"]`, defaulting to `"default"`.
- `run()`: dispatch → narrate the decision as a `Progress` **event** → orchestrate with
  `ctx.grant.capabilities` (E1) → map the `Disposition` through the existing `terminal_summary()`.
- Records its dialogue as **turns** (the goal, the decision rationale, the outcome), like any pack.

**Ships**: the pack exists and is tested. Nothing routes to it yet — deliberately, so E3 is a pure
cutover with a working target.

### E3 — The daemon routes through the hub

`Daemon` holds `Arc<GoalSessionHub>` instead of `Arc<dyn SessionRecordStore>`. `react()` already
builds a `GoalSpec` (`reaction_goal()`); it now resolves the grant and calls `hub.start_with_grant`.
`ReactionOutcome::Dispatched { session_id }` (D-b). **`BackgroundRun` is deleted** — it was
scaffolding for exactly this, and its job is done.

**Ships**: a cron **is** a hosted session. Joinable, cancellable, and `domain: "dispatch"` stops being
a lie — it is a registered pack.

### E4 — `delegate` routes through the hub

`DispatchBridge` calls `hub.start_with_grant(...)` and awaits terminal (the chat turn already blocks
on `orchestrator.run` today, so this is not new blocking). **`main-agent` drops its `dispatcher`,
`orchestrator` and `mcp` dependencies** — a kernel crate that had them only to run `delegate`.

**Ships**: one engine, provably — the second one has no callers left. And the kernel gets thinner,
which is the check that the convergence was real rather than an extra layer.

## 5. E5–E7 — the payoff: sessions that wait for you

### E5 — A pack can ask mid-run, and you find out about it

Three parts, and the first is the one that did not exist:

1. **An ask seam in the coding loop.** The pack could ask during intake and nowhere else. The build loop
   needs a bounded, deliberate way to stop and ask — *not* an open invitation to interrogate the human
   on every uncertainty (that would be worse than guessing). The natural trigger points, in order of
   how well-defined they are:
   - a **verifier keeps failing** the same way (the repair loop is not converging — today it burns the
     attempt budget and fails);
   - the loop hits an **explicit decision point** the contract does not settle (a schema choice, an
     API shape) — the honest version of "I need a choice";
   - a **`Propose`-class action** inside a goal session (today the orchestrator downgrades to a
     proposal note; in a hosted session it could just *ask*).

   Gated on `Capability::AskHuman`, so a session without it behaves exactly as today. **Bounded by an
   ask budget**, so a pack cannot turn into a chat.

2. **Out-of-band notification.** When a session emits `AwaitingInput` and **nobody is watching**, ping
   the human. "Nobody is watching" is not a guess — the hub's event bus knows
   (`broadcast::Sender::receiver_count() == 0`). If you have the session open in the TUI, no ping; if
   you are at work, a ping.

   The `Notifier` trait already exists, `TelegramNotifier` implements it, and — importantly —
   **`telegram-approvals::ApprovalBot` already long-polls `getUpdates` and handles typed replies**
   (`force_reply`), which is how proposal *revisions* work. Answering a session question is the same
   machinery pointed at `POST /api/goals/{id}/message`. This is a smaller change than it sounds.

3. **A long idle budget.** `max_idle_secs` exists (`GoalSpec` → hub → `InputChannel`) but is
   per-session only, with no default. Make it settable per `[[session_profiles]]`, defaulting
   generously for interactive coding sessions (hours) and staying `None`/short for crons.

**Ships**: a coding session works until it is done *or until it needs you*, tells you, waits — and
then **uses the answer**.

That last clause is the whole feature, and it is the one that is easy to fake. The first cut of E5
asked the question, waited, recorded the reply as a turn, emitted a `Progress` event reading
`retrying once with human guidance`, and then **failed the session without retrying anything**. On the
event bus that is indistinguishable from the real thing: the human is asked, the human answers, the
transcript shows it. Only the backend knows it was never told.

So the build is now a bounded **attempt loop**, and the regression test asserts against the *backend*,
not the event bus: two invocations, the second carrying the human's words in `prior_feedback`
(`a_failed_build_asks_the_human_and_retries_with_their_answer`). A companion test pins the bound —
one ask means one guided retry, not one ask per failure. Two notes on why this was possible at all:

- It needed **no new checkpoint machinery**. `CoderRunRequest` already carries `attempt` and
  `prior_feedback`, and the workspace still holds the failed attempt's changes; a guided retry *is* a
  repair attempt whose feedback line came from a human instead of a verifier.
- It was invisible because `CodingSessionPack` held a **concrete** `LiberadoLoopBackend`, so no test
  double could observe what the backend received. It now holds `Arc<dyn CoderBackend>`. A loop whose
  only observer is the thing it narrates to cannot be tested, and this is the third time in this audit
  that a test pointed at the wrong object hid a real defect.

### E6 — An awaiting session survives a restart

**This is the load-bearing risk of E5, and E5 is a lie without it.** Today `replay_file` coerces every
non-terminal goal session to `Failed` on boot — and, worse, sets `awaiting_input = false` on the way
past, so it does not merely kill the session, it **erases the fact that it was waiting for you**. An
awaiting session is `Running`, and `Running` is not terminal. So: you go to work, the daemon restarts
(crash, reboot, or — most likely — because you rebuilt it), and the session that was holding a
question for you is gone, with no trace that a question was ever asked.

```rust
// crates/session-store/src/jsonl.rs — correct for a session that was mid-execution,
// catastrophic for one that was merely parked on a human.
if header.goal.is_some() && !header.status.is_terminal() {
    header.status = SessionStatus::Failed;
    header.awaiting_input = false;
}
```

The coercion is *correct* for a session that was mid-execution: no pack is running it, packs are not
resumable, and leaving it `Running` would be a lie the UI renders forever. But a session **blocked on
a human** is a different thing: it is not mid-computation, it is *parked*, and the only state it needs
in order to continue is the answer it is waiting for.

Approaches, cheapest first — **this needs its own design pass, and I do not want to pretend otherwise**:

- **(a) Park, don't kill.** On replay, an `awaiting_input` session becomes a distinct non-terminal
  state (`Parked`) rather than `Failed`. Answering it **restarts the pack**, replaying the human turns
  already in the transcript into the input channel before the new answer. *Works cleanly for intake*
  (the intake loop is a pure function of `(goal, answers)`, which is exactly why S7 was built that
  way). **Does not work for the build loop** — re-running it would redo real filesystem work. So this
  buys the intake case and nothing else.
- **(b) Checkpoint the pack.** A pack declares a serializable resume point. General, honest, and a
  large change to `DomainPackRunner`.
- **(c) Sandbox/workspace snapshot.** The coder pack's workspace is already a git repo (S7). A
  suspend point could be a commit, and resume a checkout. Fits the coding pack specifically, and is
  the most promising for the build loop.

**Shipped**: (a). Replay now parks an `awaiting_input` session as `Parked` with the flag **preserved**,
so the fact that it is holding a question for you survives the restart. A session that was genuinely
mid-execution (not awaiting) still becomes `Failed` — that coercion was always correct.

One clarification that matters, because the analysis above overstated the problem. The "does not work
for the build loop" objection applies **only to resuming across a restart**. Within a live run there is
nothing to replay — the process never died, the workspace is right there — so the guided retry in E5
needed no checkpoint at all. What (c) buys is narrower than this section originally claimed: answering
a session that was parked mid-build *after a daemon restart* and having the build pick up. Until then,
the parked session is visible and honest about what it is; it just cannot continue the build.

### E7 — Cron parity

Crons keep `AskHuman` off by default (D-d). A schedule that *wants* to be able to ask sets a profile
that grants it and a long idle budget. Nothing to build here beyond config plumbing — noted so it is
not forgotten.

## 6. Order, and why

| # | Slice | Why here |
|---|---|---|
| E1 | Per-run capabilities on `Orchestrator` | Everything downstream claims the grant binds. Make it true *first*, or ship a decorative one. |
| E2 | `DispatchPack` | The pack, tested, with nothing routing to it — so E3 is a cutover, not a rewrite. |
| E3 | Daemon → hub; `Dispatched { session_id }`; delete `BackgroundRun` | A cron becomes joinable/cancellable. The read-only caveat dies. |
| E4 | `delegate` → hub; `main-agent` slims | The second engine has no callers left. The kernel getting thinner is the *proof* the convergence was real. |
| E5 | Ask mid-run + notify + long idle budget | The payoff. Needs E3 (a hosted session can be answered at all). |
| E6 | Survive a restart | Needs E5 to exist before its failure mode is worth engineering against. |
| E7 | Cron parity | Config only. |

E1–E4 is the substrate and is largely mechanical. **E5 is the feature.** E6 is what makes E5 honest.
