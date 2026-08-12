---
kind: plan
status: active
authority: implementation
domain: conformance
canonical_for: live-conformance-tier3
open_items: true
---

# Tier 3 live conformance — build spec

**Audience**: whoever implements Tier 3 on a branch based on `feat/webui-fixes` (or
`main` once that lands).
**Rationale**: [`live-conformance-suite.md`](live-conformance-suite.md) — read the Tier 3 section first.
That doc argues *why*. This one fixes *what to hand back*, so a review can be about the work rather
than about what the work was supposed to be.

**Branch**: `harden/live-conformance-suite`, rebased onto `feat/webui-fixes` (2026-08-01) so
`MessageNode.model` and related chat provenance are present. Do not target pre-that tip.

Where it names a field or endpoint, that field or endpoint exists today — check, don't assume.
One deliberate extension is called out below (`HookConfig.profile`).

## The one-sentence version

A binary that runs **on the homelab box beside the daemon**, exercises each execution path end to
end against the live HTTP surface, asserts an outcome that would actually be wrong if the path were
broken, exits non-zero naming the path that failed, and writes a full report every run (logs + vault
under the conformance zone). **v1 is hand-run only** — host scheduling and Telegram notify land
later, once the suite is stable enough to trust on a timer.

## Scope of this branch

**Tier 3 only.** The runner, the deploy-box config paths need (hook / profile / zone / grant —
snippets written when implementing), the one config-surface extension hooks need for profiles, and
the every-run vault report contract. **Not v1:** host cron, Liberado notify schedule, Telegram.

### Seam tests — not this branch

The parent doc's "cheap companion" (provider wire-body unit tests) already largely landed on `main`
before this branch:

- `crates/provider/src/openai_compat.rs` — `wire_body_seam_tests` plus the schema dual-arm tests
  (model, messages, tools present/absent, temperature, max_tokens, json_schema vs json_object).
- `crates/provider-openai-compat` — HTTP-capture tests for `reasoning` and the streaming path
  (`stream` / `stream_options`), which are applied at that crate rather than in `to_openai_request`.

Do **not** re-sweep that work here. If a later gap shows up in CI, fix it in a separate PR. This
branch is the live suite.

## Deliverable

One implementation track (not two PRs). Order of work inside it still matters: config +
`HookConfig.profile` first (without them several paths cannot run), then the binary, then a
**hand-run** green path on the box. Host cron / Liberado notify schedule are **not** v1.

## Where the code goes

New crate `crates/conformance`, binary `liberado-conformance`, with:

```toml
[package.metadata.liberado]
role = "tooling"
```

Precedent and shape to copy: `crates/eval` (`liberado-eval`) — a tooling-role binary that drives the
real system and reports. The layer-rules test enforces what a `tooling` crate may depend on; match
`eval`'s dependency discipline.

**A binary, not `#[ignore]`d tests.** The long-term point is a scheduled run that names which path
broke; **v1 proves that by hand** until the suite is mature enough to put on a timer. `cargo test`
output is not an alerting surface, and the runner needs to be executable on the box without a full
interactive checkout workflow. Keep `main.rs` thin over a `lib.rs` so the assertion logic is
unit-testable without a daemon.

**It talks HTTP only to the daemon.** No in-process wiring, no linking the daemon binary. Assertions
that cannot be made over the public API are findings to raise, not reasons to reach inside.

**It may write its own report files** under the conformance vault zone (and to local log paths) —
that is operator residue, not an agent grant. See *Failure reporting*.

**Where it runs**: on the **homelab box**, next to the daemon. The Windows dev machine is only for
development and must not appear in packaging, timers, paths, or "how to run this in production"
instructions. Nothing on the box may depend on the dev machine being up.

## The safety envelope — non-negotiable

Tier 3 runs against the **real deployment**: the real vault, the real crons, the real conversation
store, the real Telegram sink. `live-conformance-suite.md` already states the rule — *a conformance
suite that can damage the user's vault will get switched off* — and Tier 3 is the tier where that
rule is hardest to keep. These are the concrete forms of it:

1. **Every write the suite *causes agents to make* lands under one dedicated vault zone.** Add a
   `conformance` zone to `policy.toml`. Nothing the suite triggers may be authorised to write
   anywhere else — enforce it through the grant, not through the goal text. A goal that politely
   asks the model to stay in its lane is not an envelope.
2. **Every session the suite creates is `Visibility::Background`.** These are not the user's chats and
   must not appear in the sidebar. `ConversationStore::list` filters on `visibility.is_background()`
   as of this base; a foreground conformance session would reintroduce exactly the sidebar pollution
   that filter was added to fix.
3. **Never fire a real configured schedule.** `daily-planning`, `evening-debrief` and `weekly-review`
   all deliver their summary to the user. A suite that triggers one has sent the user a spurious
   06:55 brief at 03:00. Use a dedicated conformance **hook** with its own goal (v1); a suite-owned
   schedule is only for a later mature notify path, not for exercising user crons.
4. **Touch only what you created.** No cancel, park, delete or profile change against any id the run
   did not itself produce. The suite is a reader of everything else.
5. **Bounded residue.** Say in the PR what a run leaves behind. Expected residue: background sessions
   from exercised paths, **every-run** report notes under `conformance/reports/`, and P1b artifacts
   under `conformance/artifacts/`. **Cleanup is manual in v1** — document the paths; do not build a
   reaper yet.

Nothing in the suite may require a secret that is not already on the box.

## Config changes required (part of the deliverable)

### Deployed box today

`deploy/homelab/config/topology.toml` currently declares **no `[[hooks]]` and no
`[[session_profiles]]`**. Two of the five paths below therefore cannot be exercised against the box as
configured — not "would be flaky", *cannot run at all*.

So the deliverable includes config, and the config is part of the review:

- a `conformance` hook in `[[hooks]]`, with its own goal, optional pool, and **profile**
- a `conformance` session profile in `[[session_profiles]]`, deliberately **narrower** than the domain
  fallback so that P4 can tell them apart (see below)
- a `conformance` zone in `policy.toml`, `agent_writable`
- a grant for the conformance profile that permits exactly the conformance zone (and the MCPs the
  conformance goal actually needs — no more)

If you find yourself unable to test a path, **say so in the PR and leave the path reporting `skipped`
with a reason**. Do not quietly narrow the suite to what happened to be easy; a suite that silently
covers three paths while claiming five is worse than one that covers three and says so.

### `HookConfig.profile` — deliberate extension

`CronSchedule` already has optional `profile: Option<String>` (E7): when set, the reaction session
resolves grant/idle from that `[[session_profiles]]` entry. **`HookConfig` does not** — only `pool`
today.

**Add the same optional field to hooks** and wire it the same way schedules already do:

```toml
[[hooks]]
name = "conformance"
enabled = true
secret_ref = "LIBERADO_HOOK_CONFORMANCE_SECRET"   # env already on the box pattern
goal = "..."
profile = "conformance"   # optional; None keeps today's pool/domain grant behaviour
```

Why this is in scope for Tier 3, not a separate "nice to have":

- The suite needs a hook whose authority is the conformance grant, not the broad dispatcher grant.
- Several real hooks (and most crons) already *should* name a profile because the operator knows
  which tools they need; shipping profile on hooks without schedules would leave a second class of
  triggers. Schedules already have it — hooks catch up.

Validation: if `profile` is set, it must name an enabled `[[session_profiles]]` entry (same rules as
`CronSchedule.profile`). Default `None` is a pure additive change — existing topologies keep working.

Surface of that change: `config-loader` model + validation, boot path that stamps the reaction
session (mirror schedule handling), tests. Not a refactor of the reaction pipeline.

## The paths

For each: what to trigger, and what counts as proof. The rule from the parent doc governs everything
here — **assert the thing that would be wrong, not that nothing errored.** A `202` proved nothing on
2026-07-28; the session started and then failed every action it attempted.

### P1a — cron liveness

**Trigger**: none. This is a read.

**Assert**: for every schedule with `enabled = true`, `GET /api/reactions` contains an event from that
schedule newer than 1.5× its period.

This is the check that catches the actual defect ("both morning crons dead for a day") without firing
anything, which is why it is split out from P1b. It is the cheapest genuinely valuable check in the
suite.

**The trap**: `state.reactions` is an in-memory ring that empties on restart. Gate the assertion on
`GET /api/status` → `uptime_seconds` being greater than the period being checked, or the check fails
every time it runs after a deploy and gets muted within a week. A check that cries wolf is deleted,
and then the real cron outage is invisible again.

**Note**: enabled *user* schedules (daily-planning, etc.) are observed only — never triggered. The
conformance hook/schedule is separate.

### P1b — event → dispatch → execute

**Trigger**: `POST /api/hooks/conformance` with a fresh `run_id` in the body context and as the
idempotency key. Same event→dispatch→execute path a cron takes, minus the timer. The hook's
`profile = "conformance"` keeps the write surface inside the envelope.

**Assert**, in order of how much they prove:
- `ReactionOutcome::Dispatched { session_id }` appears for the correlation id
- `GET /api/goals/{session_id}` reaches a terminal **success**, not merely terminal
- **ground truth on disk**: `$vault/conformance/artifacts/<run_id>.md` exists and body contains
  `CONFORMANCE_OK <run_id>` (see *P1b goal + on-disk success* under Settled decisions)

**Not required (v1):** `ToolFinished { ok: true }` on the goal-session event log. The dispatch
path records `progress` + `session_finished` today, not `tool_finished` frames (live-verified
2026-08-01). Claiming ToolFinished without emission was suite theatre. The artifact on disk is the
ground-truth arm that catches "started then failed every action."

**Not proof**: `202` from the hook. A status code, a `Dispatched` outcome, and a terminal status are
all things the system says about itself.

### P2 — chat turn

**Trigger**: `POST /api/chat/stream`.

**Assert**:
- at least one `Token` delta arrived (the provider was really reached)
- `GET /api/conversations/{id}` holds a `User` message and an `Assistant` message (by id — Background
  chats are filtered from the **list** endpoint by design)
- session `visibility` is `background` on `GET /api/sessions`
- the assistant message's `model` equals the daemon's active model from `GET /api/status`, and the
  stamp **must be present** — missing stamp is a fail (not a silent pass)

That last one needs `MessageNode.model` on the store and on the **wire** (`ChatMessage.model` on
`GET /api/conversations/{id}`). It is a cross-check between two independently derived facts — the
shape §6 of `failure-modes.md` says nothing ever guards.

Chat turns the suite starts must be **Background** so they stay out of the sidebar (`ChatRequest.background`).

### P3 — hook → joinable session

**Trigger**: `POST /api/hooks/conformance` (may share P1b's run).

**Assert**: the returned session id is **joinable** — `GET /api/goals/{id}` returns it and
`GET /api/goals/{id}/stream` accepts a subscriber. The 2026-07-13/14 class of defect was precisely a
session that existed and could not be reached.

### P4 — spawn under a profile

**Trigger**: `POST /api/goals` naming the conformance profile.

**Assert**: `GET /api/goals/{id}` → `session.grant` equals the **profile's** grant, not the domain
fallback. `GoalSessionRecord.grant` is serialised onto that response today, so this needs no new
endpoint.

Make the conformance profile *strictly narrower* than the domain fallback — a profile that happens to
resolve to the same authority as the fallback makes this assertion pass no matter which one was
applied, which is the same "gate that refuses everything" mistake the parent doc warns about in the
other direction. **The profile and the fallback must be distinguishable, or the check is theatre.**

Second arm, if cheap: the session's `RoleStarted { model }` matches the profile's declared model.

### P5 — delegate

**Trigger**: a chat turn constructed to require delegation.

**Assert (v1)**: a child session exists, is `Background`, and is parent-linked to the chat that
delegated.

**Out of v1:** asserting the child carries the **dispatcher** grant. Grant fingerprints on the
sessions list interact with pool ceilings and are easy to get wrong without teaching flaky
ignores of P1–P4. Revisit when a stable, dual-arm grant assert is cheap.

**This is the one non-deterministic path** — whether the model delegates is a model decision. Report
it separately and **do not let it set the exit code** by default; put it behind a flag. A flaky gate
teaches people to ignore the gate, and then P1–P4 stop being read either.

## Output contract

- **stdout**: one JSON object per path — `{path, status: "pass"|"fail"|"skipped", duration_ms, assertion, evidence, reason}`.
  `evidence` carries the observed value that decided it; `reason` is required when `skipped`.
- **stderr**: human-readable progress.
- **exit code**: `0` only if every non-advisory path passed. Non-zero must be attributable to a named
  path from stdout alone — whoever reads this at 3am has the exit code and the log, nothing else.
- **flags/env**: base URL (required, no default pointing at production — on the box that is typically
  `http://127.0.0.1:4201` or the container-local URL, still explicit), path selection, flag to
  include advisory paths (P5) in the exit code, path to the runner's own config file.

### Runtime budget

Configurable in the **runner's own TOML** (not `topology.toml` — the daemon does not need to know
the suite's patience). Default hardcoded to **30 minutes** for the whole run if the key is absent.

```toml
# e.g. /etc/liberado/conformance.toml  (path via flag or well-known location on the box)
base_url = "http://127.0.0.1:4201"
budget_secs = 1800          # default 1800 (30 min) if omitted
# optional: per-path overrides, path allow/deny, vault report dir, etc.
```

If the budget elapses, unfinished paths are `fail` (or `skipped` with reason `budget_exhausted` —
pick one, document it, and make it visible in the report). Do not hang forever.

### Failure reporting (settled)

Three surfaces, different jobs:

| Surface | What | When |
|---|---|---|
| **stderr + process logs** | Human-readable progress and the same facts as stdout | every run |
| **stdout JSON** | Machine-readable per-path results (for wrappers, journal capture) | every run |
| **Vault report** under the `conformance` zone | Full narrative: which paths ran, assertions, evidence, correlation ids, timestamps, build SHA if available | **every run** (pass and fail) |

Vault path convention:

```
conformance/reports/YYYY-MM-DDTHHMMSSZ-<pass|fail>.md
```

The report is written by the **runner** (on the box, into the vault tree). It is not "the model wrote
a note" — agent goals still only write under their grant; the runner's report is operator tooling
residue. Cleanup of old reports is **manual in v1**.

**Telegram / host scheduling — not v1.** While dogfooding, the operator runs the binary by hand on
the box and reads stdout/stderr plus the vault report. Do **not** wire host cron, a systemd timer,
or a Liberado `conformance-notify` schedule until the suite is stable and mature. The intended mature
path is documented under Settled decisions so we do not invent a second Telegram surface later; it is
not part of the first landable version.

**v1 acceptance**: a hand-invoked run on the box that exercises the non-advisory paths, writes the
vault report, and exits non-zero on a forced failure with a named path in stdout.

## Every check must be shown to fail

§1 of `failure-modes.md`: a check that cannot fail is not a check. This applies with more force here,
because a suite that passes against a healthy box tells you nothing about whether it would notice an
unhealthy one — and you cannot break production to find out.

So: run the runner against a **locally started daemon** with the relevant thing deliberately broken —
a disabled schedule, a profile with the fallback's grant, an MCP pointed at a dead port, a hook whose
goal writes nothing — and **paste the failing output into the PR description, per path**. Reviewing
this without that evidence is guesswork, and I will ask for it.

## Out of scope — please don't

- Re-doing provider wire-body seam tests (already on main; see *Scope*).
- Modifying `crates/server/src/t1_conformance.rs`. Tier 1 is complete and passing; leave it alone.
- Refactoring daemon internals to make assertions easier. If an assertion genuinely needs a field that
  is not exposed, **stop and raise it** — that is a surface design change and wants its own
  discussion, not to arrive inside a test PR.
- Committing any secret, key, or token.
- Making this a CI gate. It needs a live box; CI does not have one.
- Widening any existing grant to make a check pass.
- Depending on the Windows dev machine for anything that runs on the box.

## What review will look at

In rough priority order:

1. **Does each assertion bottom out in ground truth**, or in the system's own report of itself? This is
   the single thing most likely to be wrong, and the reason the parent doc exists.
2. **Both arms on anything that asserts a refusal.** A check that only ever asserts "denied" passes
   against a system that denies everything.
3. **The safety envelope**, especially: can any path write outside the conformance zone, can any path
   fire a user-visible schedule, does any path create a foreground session.
4. **P4's profile is actually distinguishable from the fallback.**
5. **P1a's restart gate** — does it survive a deploy without false-failing.
6. **Failure evidence present for every check**, plus a real vault report sample for a forced fail.
7. **`HookConfig.profile` is optional and default-preserving**, wired like schedule profiles.
8. Layer rules, `cargo clippy`, `cargo test --workspace` green. Allowed change surface:
   `crates/conformance`, `crates/config-loader` (+ any thin call-site that already stamps schedule
   profiles onto reactions), `deploy/homelab/config`, docs. Raise if you need more.

Commit-message and doc style: match the surrounding repo. Explain *why* in comments where the reason
is not local — a doc comment that repeats the function signature is worse than none.

## Settled decisions (planning 2026-08-01)

| Question | Decision |
|---|---|
| Where does it run? | **On the homelab box** next to the daemon. Dev machine is development only. |
| How do we run it (v1)? | **Hand-run on the box only.** No host cron, no systemd timer, no Liberado suite/notify schedule until the suite is solid enough to trust unattended. |
| Failure notify (mature)? | **Full report** always in logs + vault. Later: short Telegram via a Liberado schedule on `deliver_cron` — no second Telegram client in the runner. **Not wired in v1.** |
| Vault reports | **Every run** writes a report (pass and fail). Silence is not a green signal. Hand-run dogfooding reads these in Obsidian. |
| Run budget? | **TOML on the runner**, default **30 minutes** (`budget_secs = 1800` if omitted). |
| Runner config path | Dedicated `conformance.toml` **next to the deploy config** (same dir as `topology.toml` / `policy.toml` on the box). |
| Seam tests / PR1? | **Out of scope** for this branch; already largely on main. |
| Hook profiles? | **Yes** — add optional `HookConfig.profile`, same semantics as schedules (put `profile` on `EventPayload.data` the way `liberado-cron` already does). |
| Base branch? | **`feat/webui-fixes`** until it merges; carries `MessageNode.model` for P2. |
| P2 background chat | Prefer existing API. If chat cannot be Background over HTTP, **add that** as a thin surface change — do not invent a parallel session path. |
| Residue cleanup | **Manual for v1** (sessions + reports). Document what accumulates. |
| P1a schedule set | Assert only **user** schedules that should have been live. **Exclude** any suite-owned schedules if/when they exist so they cannot fail P1a. |
| Deploy topology/policy snippets | Required for paths that need a hook/profile/zone — **surface concrete snippets when implementing**, not as a pre-baked novel here. |

### v1 vs mature operation

| | **v1 (this landable version)** | **Later (when stable)** |
|---|---|---|
| How the runner starts | Operator SSH / shell on the box | Host cron or systemd timer |
| Telegram | None from the suite | Liberado schedule `conformance-notify` → `deliver_cron` |
| What you read | stderr + stdout JSON + vault report | Same, plus a short Telegram pointer on fail (and optional green ping) |

### Mature Telegram path (deferred — do not build yet)

When the suite is mature enough to schedule:

1. Runner keeps writing `conformance/reports/<ts>-<pass|fail>.md` and never talks to Telegram.
2. A Liberado schedule `conformance-notify` (profile: Read on `conformance` only) fires after the suite window.
3. Its goal: open the newest report; final summary is a short line — `Tier 3 green — <path>` or
   `Tier 3 FAILED — see <path> in Obsidian` — shipped by existing `deliver_cron`.

Until then, **do not** add that schedule to deploy config just to have it sit disabled. P1a must never
require suite-owned schedules to have fired.

### Runner config location

On the box, next to deploy config, e.g.:

```
<LIBERADO_CONFIG_DIR>/conformance.toml   # same directory as topology.toml / policy.toml
```

Committed template under `deploy/homelab/config/conformance.toml`. The runner accepts `--config` and defaults to that well-known relative name beside the daemon's config dir when run on the box.

### P1b goal + on-disk success (recommendation, settled as the plan)

Requirements the goal has to satisfy:

- Exercises **event → dispatch → execute → real tool write** (the 28th's blind spot).
- Ground truth the runner can check **without trusting session status**.
- Unique per run so last night's pass cannot green-light tonight.
- Fits the **conformance** grant only (turbovault write into that zone).
- Boring for the model — low judgment, low flakiness.

**Mechanism** (already on the wire today):

- Runner mints `run_id` (ULID).
- `POST /api/hooks/conformance` with header `X-Liberado-Idempotency-Key: <run_id>` and body
  `{"context":"run_id=<run_id>"}` — the handler already appends that as
  `Additional context from the trigger: …`.
- Hook `profile = "conformance"`.

**Configured hook goal** (static text in `topology.toml`):

```text
You are a live-conformance probe. Do exactly this and nothing else:

1. From the "Additional context from the trigger" line, read the run_id value
   (the token after "run_id=").
2. Using turbovault write_note, create exactly one note:
   - path: conformance/artifacts/<run_id>.md
   - body: a single line exactly: CONFORMANCE_OK <run_id>
3. Do not write any other path. Do not ask the human. Do not search the web.
4. When the write succeeds, finish with a one-line summary that repeats the path.
```

**Runner proof** (in order — later steps only if earlier pass):

1. Hook accepted with that `correlation_id` / idempotency key.
2. `Dispatched { session_id }` for that correlation.
3. Session reaches terminal **Succeeded**.
4. **Ground truth**: file exists at
   `$vault/conformance/artifacts/<run_id>.md` and its body contains
   `CONFORMANCE_OK <run_id>` (runner reads the vault filesystem — it is on the box).

ToolFinished on the hub event log is **not** a v1 requirement (dispatch path does not emit it today).

**Why this shape**

| Alternative | Why not |
|---|---|
| Fixed path `conformance/artifacts/last-run.md` | Prior run can satisfy a later assertion unless mtime is perfect; unique path is simpler. |
| Assert only session status / tool events | Exactly the self-report trap from 2026-07-28. |
| Fancy multi-MCP goal (weather + calendar + …) | Proves product goals, not the plumbing; flaky for the wrong reasons. |
| Model invents the path | Runner would not know where to look without parsing the summary. |

**Grant for profile `conformance`**: `Write { Vault = "conformance" }`, `Read { Vault = "conformance" }`, `ExecuteMcp = "turbovault"` — nothing else. Zone `conformance` is `agent_writable`. Artifact + reports both live under that zone so the envelope is one directory tree:

```
conformance/
  artifacts/<run_id>.md     # P1b ground truth (agent-written)
  reports/<ts>-<pass|fail>.md  # suite report (runner-written, every run)
```

### P2 background chat

Confirm whether `POST /api/chat/stream` (or an adjacent goals/chat create) can stamp `Visibility::Background`. If yes, use it. If not, add the smallest existing-shaped knob (e.g. request field or suite-only header that maps to the same `start_background` / visibility path goal sessions already use). No second store, no parallel chat stack.

## Still open (implementation detail, not design forks)

- When promoting off hand-run: host cron cadence, and whether to wire `conformance-notify` (and keep
  green Telegram pings vs fail-only later).
- P5 dispatcher-grant arm (deferred from v1).
- Optional: emit real `tool_finished` on the dispatch hub path and re-add as a P1b assert later.
