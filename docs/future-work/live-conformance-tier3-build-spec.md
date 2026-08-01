# Tier 3 live conformance — build spec

**Audience**: whoever implements Tier 3 on a branch off `main`.
**Rationale**: [`live-conformance-suite.md`](live-conformance-suite.md) — read the Tier 3 section first.
That doc argues *why*. This one fixes *what to hand back*, so a review can be about the work rather
than about what the work was supposed to be.

Written 2026-08-01 against `main` at the time. Where it names a field or endpoint, that field or
endpoint exists today — check, don't assume, but nothing here is aspirational.

## The one-sentence version

A binary that, pointed at a **running deployed daemon**, exercises each execution path end to end,
asserts an outcome that would actually be wrong if the path were broken, and exits non-zero naming
the path that failed.

## Deliverables

Please split into **two PRs**. They are different kinds of work and the first is nearly free.

### PR 1 — seam tests (do this first)

Unit tests inside the crates that own each provider-facing boundary, asserting on the **built request
body**. `crates/provider/src/openai_compat.rs` already has two
(`a_schema_that_constrains_shape_is_sent_as_json_schema`, `a_shapeless_schema_falls_back_to_json_object`);
this is a sweep of that class, not a new pattern.

Cover, at minimum: tools/function definitions, `temperature`, `max_tokens`, `reasoning`, streaming
flag, and the system/user message ordering. One test per thing that could silently stop being sent.

No new crate, no config changes, runs in CI. This is the PR that would have caught the schema defect
years before a live run would.

### PR 2 — the Tier 3 runner

Everything below.

## Where the code goes

New crate `crates/conformance`, binary `liberado-conformance`, with:

```toml
[package.metadata.liberado]
role = "tooling"
```

Precedent and shape to copy: `crates/eval` (`liberado-eval`) — a tooling-role binary that drives the
real system and reports. The layer-rules test enforces what a `tooling` crate may depend on; match
`eval`'s dependency discipline.

**A binary, not `#[ignore]`d tests.** The point of this suite is to run on a schedule and tell you
which path broke. `cargo test` output is not an alerting surface, and the runner needs to be
executable somewhere that is not a checkout with a toolchain. Keep `main.rs` thin over a `lib.rs`
so the assertion logic is unit-testable without a daemon.

**It talks HTTP only.** No in-process wiring, no linking the daemon. If an assertion cannot be made
over the public API, that is a finding to raise (see *Open questions*), not a reason to reach inside.

## The safety envelope — non-negotiable

Tier 3 runs against the **real deployment**: the real vault, the real crons, the real conversation
store, the real Telegram sink. `live-conformance-suite.md` already states the rule — *a conformance
suite that can damage the user's vault will get switched off* — and Tier 3 is the tier where that
rule is hardest to keep. These are the concrete forms of it:

1. **Every write the suite causes lands under one dedicated vault zone.** Add a `conformance` zone to
   `policy.toml`. Nothing the suite triggers may be authorised to write anywhere else — enforce it
   through the grant, not through the goal text. A goal that politely asks the model to stay in its
   lane is not an envelope.
2. **Every session the suite creates is `Visibility::Background`.** These are not the user's chats and
   must not appear in the sidebar. `ConversationStore::list` filters on `visibility.is_background()`
   as of `main`; a foreground conformance session would reintroduce exactly the sidebar pollution
   that filter was added to fix.
3. **Never fire a real configured schedule.** `daily-planning`, `evening-debrief` and `weekly-review`
   all deliver their summary to the user. A suite that triggers one has sent the user a spurious
   06:55 brief at 03:00. Use a dedicated conformance schedule/hook with its own goal.
4. **Touch only what you created.** No cancel, park, delete or profile change against any id the run
   did not itself produce. The suite is a reader of everything else.
5. **Bounded residue.** Say in the PR what a run leaves behind and how it is reclaimed. "Nothing" is a
   fine answer; "a background session per path per night, forever" needs a cleanup story.

Nothing in the suite may require a secret that is not already on the box.

## Prerequisites on the deployed box — read this before planning

The deployed `deploy/homelab/config/topology.toml` currently declares **no `[[hooks]]` and no
`[[session_profiles]]`**. Two of the five paths below therefore cannot be exercised against the box as
configured — not "would be flaky", *cannot run at all*.

So the deliverable includes config, and the config is part of the review:

- a `conformance` hook in `[[hooks]]`, with its own goal and its own pool
- a `conformance` session profile in `[[session_profiles]]`, deliberately **narrower** than the domain
  fallback so that P4 can tell them apart (see below)
- a `conformance` zone in `policy.toml`, `agent_writable`
- a grant for the conformance profile that permits exactly the conformance zone

If you find yourself unable to test a path, **say so in the PR and leave the path reporting `skipped`
with a reason**. Do not quietly narrow the suite to what happened to be easy; a suite that silently
covers three paths while claiming five is worse than one that covers three and says so.

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

### P1b — event → dispatch → execute

**Trigger**: `POST /api/hooks/conformance`. Same event→dispatch→execute path a cron takes, minus the
timer.

**Assert**, in order of how much they prove:
- `ReactionOutcome::Dispatched { session_id }` appears for the correlation id
- `GET /api/goals/{session_id}` reaches a terminal **success**, not merely terminal
- the transcript contains at least one `ToolFinished { ok: true }` — the 28th's failure mode was a
  session that started and then failed every action, which a status check alone reads as fine
- the artifact the goal was told to produce exists

**Not proof**: `202` from the hook. A status code, a `Dispatched` outcome, and a terminal status are
all things the system says about itself.

### P2 — chat turn

**Trigger**: `POST /api/chat/stream`.

**Assert**:
- at least one `Token` delta arrived (the provider was really reached)
- the conversation appears in `GET /api/conversations` afterwards
- `GET /api/conversations/{id}` holds a `User` node and an `Assistant` node
- the assistant node's `model` equals the daemon's active model from `GET /api/status`

That last one is new as of `main` (`MessageNode.model`) and is worth having: it is a cross-check
between two independently derived facts, which is the shape §6 of `failure-modes.md` says nothing ever
guards. It also catches "the request never reached a provider" more sharply than token presence does.

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

**Assert**: a child session exists, is `Background`, and carries the **dispatcher** grant.

**This is the one non-deterministic path** — whether the model delegates is a model decision. Report
it separately and **do not let it set the exit code** by default; put it behind a flag. A flaky gate
teaches people to ignore the gate, and then P1–P4 stop being read either.

## Output contract

- **stdout**: one JSON object per path — `{path, status: "pass"|"fail"|"skipped", duration_ms, assertion, evidence, reason}`.
  `evidence` carries the observed value that decided it; `reason` is required when `skipped`.
- **stderr**: human-readable progress.
- **exit code**: `0` only if every non-advisory path passed. Non-zero must be attributable to a named
  path from stdout alone — whoever reads this at 3am has the exit code and the log, nothing else.
- **flags/env**: base URL (required, no default pointing at production), per-path timeout, path
  selection, and a flag to include advisory paths in the exit code.

Scheduling it (systemd timer, cron, whatever) is **out of scope for PR 2** — get the runner correct
and runnable by hand first. A green manual run is the acceptance criterion.

## Every check must be shown to fail

§1 of `failure-modes.md`: a check that cannot fail is not a check. This applies with more force here,
because a suite that passes against a healthy box tells you nothing about whether it would notice an
unhealthy one — and you cannot break production to find out.

So: run the runner against a **locally started daemon** with the relevant thing deliberately broken —
a disabled schedule, a profile with the fallback's grant, an MCP pointed at a dead port, a hook whose
goal writes nothing — and **paste the failing output into the PR description, per path**. Reviewing
this without that evidence is guesswork, and I will ask for it.

## Out of scope — please don't

- Modifying `crates/server/src/t1_conformance.rs`. Tier 1 is complete and passing; leave it alone.
- Refactoring daemon internals to make assertions easier. If an assertion genuinely needs a field that
  is not exposed, **stop and raise it** — that is a surface design change and wants its own
  discussion, not to arrive inside a test PR.
- Committing any secret, key, or token.
- Making this a CI gate. It needs a live box; CI does not have one.
- Widening any existing grant to make a check pass.

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
6. **Failure evidence present for every check.**
7. Layer rules, `cargo clippy`, `cargo test --workspace` green, no changes outside `crates/conformance`
   + `deploy/homelab/config` + docs.

Commit-message and doc style: match the surrounding repo. Explain *why* in comments where the reason
is not local — a doc comment that repeats the function signature is worse than none.

## Open questions — decide with the human, don't guess

- **Where does it run from?** The dev machine over the tailnet, or on the homelab box beside the
  daemon? Affects packaging and nothing else, but it does affect packaging.
- **Should a failure notify anything?** Exit code only, or a Telegram message through the existing
  sink? Notifying is more useful and more dangerous; default to exit code and ask.
- **How long may a run take?** P1b and P5 involve real model calls at real latency. If the budget is
  minutes rather than tens of minutes, P5 is the first thing to drop.
