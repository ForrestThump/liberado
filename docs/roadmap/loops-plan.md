# Loops — plan (scheduled recurrence over goals)

**Status**: plan, 2026-07-12 — no code yet.  
**The one-sentence architecture decision**: **a loop is a scheduler for goals, not a fourth
engine.** Goals terminate; loops recur; a loop's body is an ordinary goal session. Loops therefore
inherit verifiers, capability narrowing, hats, transcripts, and (later) interactivity for free,
and the kernel never grows a second lifecycle.  
**Vocabulary**: [`agentic-loops.md`](../architecture/agentic-loops.md) §Vocabulary — *turn loop*
(executor) ⊂ *goal* (run-to-terminal) ⊂ *loop* (this doc) ⊂ *meta-loop* (tuner).  
**Related**: [`session-focus-plan.md`](session-focus-plan.md) ·
[`verifiers.md`](../architecture/verifiers.md) · cron/`EventSource` (Decision 18/19) ·
external reference: the `/goal` vs `/loop` convention
([`loop_architecture_reference_article.md`](../ideas/loop_architecture_reference_article.md)).

---

## 1. What a loop is (and is not)

Time-based recurrence: wake on a schedule → inspect current state → make **one useful improvement
pass** → log what changed → sleep. A loop **never succeeds closed** — it ends by cap, by its
checker being satisfied N consecutive times, or by a human closing it. Canonical uses: keep a
document tightened, keep an inbox triaged, keep a dataset/vault section groomed, re-run a research
sweep weekly.

Not a loop: anything with a completion state ("build X", "fix Y") — that's a **goal**. If a
recurring job would be "done" after one good run, it's a goal on a cron, and today's machinery
already handles it. What today's machinery does **not** have is *series memory*: each cron firing
is a one-shot reaction that knows nothing about the previous firing. Series memory is the entire
gap this plan fills.

## 2. Why Liberado is unusually well positioned for loops

The reference framework's four components map onto existing pieces almost 1:1:

| Component | Liberado piece |
|---|---|
| **Artifact** (the thing improved each pass) | A vault note/doc — and Decision-5 **loop-breaking** means a loop's own edits never re-trigger the watch pipeline. Artifact-improvement loops are feedback-storm-safe *by construction*, which generic harnesses cannot claim |
| **Checker** (did the pass help?) | The `Verifier` pipeline — frozen specs, deterministic, domain-agnostic (`content_contains`, `command`, future `http`) |
| **Cap** | Existing budget story (`ResourceLimit`, attempts) lifted one level: max firings per series + per-pass budgets |
| **Human gate** | Proposal flow / draft-PR gates, already wired to Telegram |

Also already built: the trigger (`liberado-cron` + `Topology.schedules` + pools for authority),
the body (goal sessions + domain packs + hats), and the record (session transcripts + the
converged event vocabulary, so a surface can replay any pass).

## 3. The composition

```
[[loops]] entry (topology)            ── series definition (config, human-owned)
   schedule  ── cron expr (reuses CronSchedule; pool = authority, as today)
   goal template ── description + domain/hat + payload (artifact ref, changelog ref)
   checker  ── VerifierSpec[] evaluated per pass (the pass's goal verifiers)
   caps     ── max_passes, per-pass budget, max_consecutive_failures
   stop_when ── checker_green_streak(N) | cap | human_close

LoopSeries (runtime state, durable)   ── the NEW piece
   loop_id, pass_count, last_pass_at
   changelog ref (append-only; each pass's GoalResult.summary + verifier verdicts)
   green_streak, consecutive_failures
   status: Active | Paused | Closed { reason }

each firing:
   cron fires → loop runner loads LoopSeries → stop_when already met? close, notify
   → else spawn ORDINARY goal session (template + artifact + changelog tail as context)
   → session runs to terminal (verifiers = checker) → append pass record to changelog
   → update streaks/counts → evaluate stop_when → close + notify, or sleep
```

Key properties this buys:

- **One improvement pass per firing** is just a goal with a tight budget — drift control,
  maker≠checker, and named terminals all come from the goal kernel unchanged.
- **The changelog is the series' working memory** (the "State" row of the goal table, lifted to
  series level): each pass sees what previous passes did, so it doesn't redo or undo them.
- **Authority**: a loop's passes run under the schedule's pool component — a loop can never do
  anything its pool couldn't do once. Nothing new to prove.
- **Interactivity later for free**: once session-focus lands, a human can *join a pass in flight*
  (or a pass can end `NeedsHumanReview` and notify) with zero loop-specific work.

## 4. Gaps (the honest list — smaller than it looks)

| # | Gap | Where it lands |
|---|---|---|
| L1 | `[[loops]]` config: schedule + goal template + checker + caps + stop_when. Template's pack payload stays **opaque** to the config stack (same rule as `[tuning.coder]`) | `config-loader` (shape), pack parses its own payload |
| L2 | `LoopSeries` durable state + changelog (append-only JSONL under `<LIBERADO_DATA_DIR>/loops/` — operational data, outside the vault; same reasoning as Decisions 12/17). The *artifact* lives in the vault; the *bookkeeping* does not | new module — likely in `liberado-session` (series is kernel vocabulary) or a thin `loop` module beside the hub; **not** a new engine crate |
| L3 | The loop runner: on cron fire, resolve series → spawn goal → collect result → update series → evaluate stop. Rides the existing `EventPayload.pool` routing; the daemon side is a consumer of the hub, like the server is | daemon/server composition root |
| L4 | Pass context assembly: goal description = template + artifact ref + changelog tail (last K entries, capped — context-efficiency pillar applies to loops too) | loop runner |
| L5 | Close/notify: series close (any reason) goes through `Notifier`; `Paused` for human investigation (e.g. `max_consecutive_failures` hit) | loop runner + notify |
| L6 | Surfaces: list loops + series state + changelog view; TUI first (reuses session browser patterns); a pass is a normal goal session, so its transcript is already renderable | TUI, `/api/loops*` (thin read API + pause/close) |

Explicitly **not** needed: a new execution engine, new event vocabulary (passes emit
`SessionEvent`s), new capability machinery, or any change to packs — a pack cannot tell whether
its goal was spawned by a human, a dispatch, or a loop. That indistinguishability is the design
test for L1–L4: if a pack needs a "loop mode," the composition is wrong.

## 5. Slices

| # | Slice | Proves it with |
|---|---|---|
| P1 | L1+L2: config shape + durable `LoopSeries`/changelog (no runner yet — series creatable/listable) | unit tests; fail-fast validation of `[[loops]]` |
| P2 | L3+L4: runner wired to cron; passes spawn goal sessions; changelog grows; stop_when closes | daemon integration test: 2-pass loop on the life pack with a `content_contains` checker, closes on green streak |
| P3 | L5: notify on close/pause; `max_consecutive_failures` → `Paused` | live Telegram smoke |
| P4 | L6: `/api/loops*` + TUI list/changelog view | dogfood: a real vault-note-tightening loop, run for a week |
| P5 | (After session-focus S4) join-a-pass + `NeedsHumanReview` pass terminals | the "call transfer" applied to a loop pass |

P4's dogfood loop is the acceptance test for the whole concept: artifact = a real vault doc,
checker = content/structure verifiers, cap = 10 passes, human gate = review the changelog.

## 6. Open questions

1. **Where does `LoopSeries` live** — `liberado-session` (series as kernel vocabulary, keeps one
   home for session-shaped state) vs a module in the daemon (loops as a trigger concern, like
   cron)? Lean: `liberado-session`, since surfaces will want to render series state through the
   same store patterns.
2. **Overlap policy** — if a pass is still running when the schedule fires again: skip (default,
   log it) vs queue. Lean skip; loops are low-frequency by nature.
3. **Checker placement** — per-pass verifiers (each pass judged alone) vs series-level checker run
   against the artifact after the pass. These differ once passes can fail without harming the
   artifact. Lean: they're the same `VerifierSpec` list, run as the pass's goal verifiers, with
   `green_streak` computed from pass outcomes.
4. **Dispatcher-spawned loops** — can chat create a loop ("keep my weekly-review note groomed")?
   Later; v1 loops are config-authored (`topology.toml` edit = human-owned, Decision 14). A
   `ProposeLoop` path can ride the proposal flow when wanted.
5. **Naming collision tolerance** — "loop" coexists with doom-loop/loop-break/turn loop; accepted
   deliberately (2026-07-12, matches `/loop` convention in other harnesses). The vocabulary table
   in `agentic-loops.md` is the disambiguation point.

## 7. Docs to touch when implementing

`agentic-loops.md` (flip the vocabulary table's loop row from "designed" to "built"),
`overview.md` (crate map row if a module becomes visible; cron row gains "loops ride this"),
`contracts.md` (only if `LoopSpec`/series types become a public seam), `api.md` (`/api/loops*`),
`current.md` (slice tracking).
