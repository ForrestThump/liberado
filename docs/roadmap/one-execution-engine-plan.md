# One execution engine — sketch

**Status**: sketch, 2026-07-13. Not started. Decisions needed (§6) before code.
**Debt item**: #1 from the 2026-07-13 hygiene audit ([`current.md`](current.md)).
**Related**: [`../architecture/sessions.md`](../architecture/sessions.md) ·
[`agentic-loops.md`](../architecture/agentic-loops.md) ·
[`channels-and-interactivity.md`](../architecture/channels-and-interactivity.md)

---

## 1. The problem, stated plainly

D7 unified how sessions are **stored and displayed**. It did not unify how they are **run**. There
are two engines:

| Engine | Runs | Entered by |
|---|---|---|
| `GoalSessionHub` + `DomainPackRunner` | goal sessions (coding, life) | `/spawn`, `POST /api/goals` |
| `Dispatcher` + `Orchestrator` | daemon reactions, `delegate` | cron, webhooks, vault changes, the face agent's `delegate` tool |

Everything that runs on the second engine is **recorded, not hosted**. That is why:

- a background session's `domain` is the fake value `"dispatch"` — no pack ran it;
- **joining one is read-only.** You can watch a cron work; you cannot answer it, steer it, or cancel it;
- a `Clarify` is a **dead end**. The reaction needed a human, there wasn't one, and it dies `Failed`
  with the questions stuffed into a summary string. Nobody can answer them, ever;
- authority is described two ways: a `SessionGrant` (hub) and pool `capabilities` (daemon);
- `main-agent` — a *kernel* crate — carries `dispatcher` + `orchestrator` + `mcp` deps solely to run
  `delegate`.

## 2. The realization this rests on

**The dispatcher + orchestrator pair is already a `DomainPackRunner` in everything but name.** Compare:

```
DomainPackRunner::run(session_id, goal, ctx, events, inputs, cancel) -> Result<GoalResult, PackError>

dispatch/orchestrate:  goal -> DispatchDecision -> Disposition
                       Disposition::terminal_summary() -> (TerminalKind, String)  // already exists
```

Every input it needs is already on the session: the goal text is `goal.description`, the authority is
`ctx.grant.capabilities`, the correlation id is `goal.origin.correlation_id`, the catalog and
zone-write-classes are pack-held config. It produces a terminal result.

So this is not "build a third thing". It is **`DispatchPack: DomainPackRunner`** — a new `pack`-role
crate wrapping the dispatcher and orchestrator — after which cron, webhooks, and `delegate` all start
sessions the same way `/spawn` does, and the second engine stops existing.

## 3. What it buys

1. **Background sessions become real sessions.** Joinable, cancellable, and — with `AskHuman` —
   *answerable*. The read-only caveat in `sessions.md` goes away.
2. **`Clarify` stops being a dead end.** A hosted session that needs a human can emit `AwaitingInput`
   and wait, instead of dying with its questions in a string. Paired with the existing `Notifier`
   (Telegram), an unattended cron could actually *ask you* and get an answer. This is the largest
   user-visible win, and it is the one the current architecture structurally cannot deliver.
3. **One authority model.** `SessionGrant` everywhere; pools become a grant component, which is what
   they already are in `policy.toml`.
4. **`domain: "dispatch"` stops being a lie** — it becomes a real registered pack.
5. **`main-agent` sheds `dispatcher`/`orchestrator`/`mcp`.** It would hold only the hub. A kernel
   crate gets meaningfully thinner.
6. Reactions could run **concurrently** (see §5.2), which they cannot today.

## 4. The shape

```
                    ┌───────────────────────────────┐
  /spawn ──────────►│                               │──► CodingSessionPack
  POST /api/goals   │        GoalSessionHub         │──► LifeOpsDemoRunner
  cron ────────────►│  (the ONE execution engine)   │──► DispatchPack ──┐
  webhook ─────────►│                               │                   │
  delegate ────────►│                               │   Dispatcher + Orchestrator
                    └───────────────────────────────┘   (per pool)
```

`DispatchPack` (new crate, role `pack`, deps: `session`, `dispatcher`, `orchestrator`, `common`):

- holds `HashMap<pool_name, (Dispatcher, Orchestrator)>` — pools stay, because an `Orchestrator` owns
  an `McpRegistry` that is not shareable across instances;
- reads the pool from `goal.payload["pool"]`, defaulting to `"default"`;
- `run()`: dispatch → narrate the decision as a `Progress` **event** → orchestrate → map the
  `Disposition` through the existing `terminal_summary()` → `GoalResult`;
- on `Clarify`: **if `ctx.can(&Capability::AskHuman)`**, ask through `inputs` and feed the answer back
  as a re-dispatch; otherwise terminate `Failed` exactly as today. Interactivity stays a *capability*,
  which is Decision A.

The daemon then holds an `Arc<GoalSessionHub>` instead of an `Arc<dyn SessionRecordStore>`, and
`react()` becomes: build the `GoalSpec` (it already does — `reaction_goal()`), resolve the grant,
`hub.start_with_grant(...)`. `BackgroundRun` is deleted; it was scaffolding for exactly this.

## 5. The three things that are actually hard

### 5.1 The orchestrator's authority is fixed at construction

`Orchestrator::run` gates on `self.capabilities` — set in `new()`. A session grant that is *narrower*
than its pool (which is the entire point of a session profile) **cannot currently be honored**. Two ways:

- **(a)** Accept it for now: a dispatch session's grant *is* its pool's grant. Simple, ships fast,
  but leaves "one authority model" half-true and a `[[session_profiles]]` entry silently can't narrow
  a dispatch session.
- **(b)** Thread capabilities through `Orchestrator::run(..., capabilities)` and intersect with
  `self.capabilities` (**narrow-only**, per Decision 4). This is the honest fix and it is what makes
  claim #3 above real. Moderate: touches the guard pipeline and `RiskGatedToolRuntime` construction.

I recommend **(b)**, but as its own slice, *before* the pack — because doing it after means shipping a
grant that looks enforced and isn't, which is the exact class of bug this audit keeps finding.

### 5.2 `/api/reactions` semantics change

Today `react()` runs the work inline and returns `Observed | Decided | Acted`. Hosted, it *starts a
session* and the work finishes later. Options:

- **(a)** `react()` awaits the session's terminal state. Preserves the wire exactly. Keeps reactions
  serialized (they already are — one at a time, awaited in the event loop).
- **(b)** Add `ReactionOutcome::Dispatched { session_id }` (an **additive** wire change, per D5) and
  let reactions run concurrently. The reactions feed then links to a *joinable session* instead of
  reporting a flat outcome string — strictly more useful.

I recommend **(b)**. It is more honest about what actually happened, it makes the reactions feed a
navigation surface instead of a log, and it removes a serialization limit nobody chose.

### 5.3 `delegate` is synchronous inside a chat turn

The face agent calls `delegate` and blocks for a report. Hosted, it would `hub.start_with_grant(...)`
and await terminal — the turn already blocks on `orchestrator.run` today, so this is not new blocking.
The subtlety is `AskHuman`: a delegated session that wants to ask a question would need the *chat* to
route it, which is a real interaction-design question (the chat turn is mid-flight). **Recommend:
delegated sessions run without `AskHuman` in slice 1** — behavior identical to today — and revisit as
its own slice once background clarification works for cron.

## 6. Decisions I need from you

1. **Authority (§5.1)** — do (b) properly first (one more slice, but the grant actually binds), or
   accept (a) and note the gap?
2. **Reactions (§5.2)** — preserve the wire exactly (a), or additive `Dispatched { session_id }` and
   concurrent reactions (b)?
3. **Scope of the payoff** — is "a cron that gets stuck can ask you (via Telegram) and you answer it"
   something you actually want? It is the biggest thing this unlocks, and it is *also* the thing that
   could make an unattended session sit awaiting forever (the idle budget kills it, but still). If you
   do not want it, this whole change is worth much less and we should weigh it again.

## 7. Suggested slices

| # | Slice | Ships |
|---|---|---|
| E1 | `Orchestrator::run` takes per-run capabilities, intersected narrow-only with its own | The grant actually binds a dispatch execution (§5.1) |
| E2 | `liberado-dispatch-pack`: `DispatchPack: DomainPackRunner`, pool-aware, registered in the hub | The pack exists and is tested; nothing routes to it yet |
| E3 | Daemon reactions route through the hub; `BackgroundRun` deleted; `ReactionOutcome::Dispatched` | A cron **is** a hosted session: joinable, cancellable |
| E4 | `delegate` routes through the hub; `main-agent` drops `dispatcher`/`orchestrator`/`mcp` | One engine; a kernel crate gets thinner |
| E5 | `Clarify` → `AwaitingInput` when the grant permits, + `Notifier` on a background ask | A stuck cron asks you instead of dying |

E1–E3 are the spine. E5 is the payoff. E4 is the cleanup that proves the convergence is real.
