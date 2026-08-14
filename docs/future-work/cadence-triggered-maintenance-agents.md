---
kind: plan
status: draft
authority: advisory
domain: ops
canonical_for: cadence-maintenance-agents
open_items: false
---

# Cadence-triggered maintenance agents

**Status**: Idea, recorded 2026-08-10. Not scheduled. No code. The architecture audit below is
current as of `ec506ab`; verify before building.

**Owner's framing:** *"Automatic dispatch with some specific skill, for some configurable number of
commits and/or PRs completed. So basically automatically dispatch an agent to just do doc updates,
or just do test coverage checks, or mutations, or critical analysis of architecture, so that those
things run and end up in the PR pipeline without either of us having to dispatch them."*

---

## Why it is worth recording

The work this would automate is real and it is currently done by hand, late, or not at all:

- **Doc drift.** On 2026-08-10 four roadmap items were found already shipped, some for weeks — and
  the correction only happened because someone went looking. That cost a wasted dispatch.
- **Mutation coverage.** Every mutation run in the recent harness work was driven by hand. Two
  mutations silently failed to apply and were only caught because a human asserted on the needle.
- **Test-coverage gaps.** PR #75 was a one-off sweep. There is no cadence.
- **Architecture critique.** Nothing does this on any schedule.

Each is periodic, low-urgency, bounded, and reviewable as a PR — which is the exact shape that
suits an unattended agent. None of them is urgent enough that a human remembers to ask.

---

## Architecture audit — what already exists

**Most of the spine is built.** This is not a new subsystem; it is one new event source and one new
binding.

| Piece | Where | State |
|---|---|---|
| Pluggable trigger interface | `EventSource` — [`crates/common/src/event.rs`](../../crates/common/src/event.rs) | **Exists.** Two implementors: `CronEventSource`, `VaultEventSource`. |
| Event envelope with room for structured data | `Event` + `EventPayload.data: serde_json::Value` | **Exists.** Also carries `pool` for executor routing. |
| Idempotency / loop-breaking | `Event.correlation_id`, required on every event (Decision 6) | **Exists.** Load-bearing here — see [the hazard](#the-one-real-hazard-self-triggering). |
| Unattended session spawn | `GoalSessionHub::start_background` — [`crates/session/src/hub.rs`](../../crates/session/src/hub.rs) | **Exists.** |
| Routing a goal to the coding pack | `DomainHint::Coding` — [`crates/session/src/goal.rs`](../../crates/session/src/goal.rs) | **Exists, and is the default.** |
| Playbooks as files | [`Skills/`](../../Skills/) — `cold-review-pr.md`, `dream.md` | **Exists**, but nothing binds a trigger to one. |
| Driving an agent PR to ready-or-blocked | `liberado shepherd` | **Exists.** |
| Bounded concurrency for agent sessions | fan-out cap of 3 (S6, PR #72) | **Exists**, reusable. |
| Per-schedule turn budget and delivery silencing | `Schedule.max_turns`, `Schedule.deliver` | **Exists** on cron — a maintenance schedule that usually finds nothing already has a way not to spam. |

**Read `CronEventSource` before writing anything.** A cadence trigger is its sibling: same trait,
same envelope, a different question about when to fire. `Schedule.deliver` in particular was added
for exactly this class of job and its rationale is worth reading.

## What is missing — three things

**1. A `RepoEventSource`.** Watches repository state rather than the clock: commits on `main` since
the last fire, or PRs merged. Emits an `Event` with the count and the range in `payload.data`. It is
a sibling of `CronEventSource`, and it should probably *be* driven by a cron tick internally —
"check every 15 minutes whether 10 commits have landed" is simpler and more robust than hooking git,
and it does not need a webhook or a daemon on the GitHub side.

**2. A trigger → skill binding.** `Schedule.goal` is a free-text `String`. This wants
`skill: String` resolved against `Skills/`, so the prompt lives in a reviewable file rather than in
config. That also makes the skill tunable without a restart — the same reasoning that moved coder
prompts out of the binary in PR #107.

**3. Durable counter state.** "Every N commits" needs a high-water mark that survives a restart, or
a redeploy re-fires every job. `<LIBERADO_DATA_DIR>/` is the established home for state that is
*not* in the vault and that no MCP mounts — the approval ledger set that precedent.

## The one real hazard: self-triggering

**A doc-update agent's own merged PR is a commit, which advances the counter, which fires the
doc-update agent.** This is the failure that would make the feature actively harmful rather than
merely disappointing, and it must be designed in from the start, not patched later.

Three mitigations, cheapest first:

- **Exclude the agents' own commits from the count.** They are already attributable — the coding
  pack commits as `Liberado Coding Pack <coding-pack@liberado.local>`. A counter that ignores that
  author cannot be advanced by its own output.
- **Use `correlation_id` as the idempotency key** for the fire, which is what Decision 6 already
  requires of every handler.
- **A floor on wall-clock time between fires**, independent of the count, so a burst of merges
  cannot produce a burst of agents.

The first is sufficient on its own and is a few lines. The other two are cheap insurance.

## Other things to settle before building

- **Cost.** Each fire is a full coding run. At current pricing that is real money on a repo with an
  active merge rate, and the trigger is by construction unattended. A per-day cap belongs in the
  config, not in a prompt.
- **Concurrency.** Four maintenance agents firing at once on one machine will exhaust the disk —
  this has already happened once from ordinary use. Reuse the fan-out cap of 3, or lower.
- **Which skills first.** Doc updates are the obvious opener: low blast radius, easy to review,
  and the drift is demonstrated. Architecture critique is the *worst* opener — its output is prose
  nobody has to act on, which is how a feature quietly becomes noise.
- **What a "no work needed" run does.** It must exit without opening a PR, and without a
  notification. `Schedule.deliver: Some(false)` is the existing answer to the second half.

## Do not build this yet

**The harness cannot currently measure its own runs.** As of 2026-08-10 the trace is incomplete for
multi-attempt runs — 122 tool calls on the wire, 76 recorded — and three harness bugs are open
including one where a completed run is discarded because a reviewer call returned empty
([`coder-harness-reliability-2026-08.md`](coder-harness-reliability-2026-08.md)).

Automating dispatch multiplies whatever the harness currently does, including the parts that are
wrong, and removes the human who would notice. The prerequisite is not more capability; it is
knowing what a run actually did. Fix the trace gap first.

---

## Sketch, for whoever picks this up

```toml
[[maintenance]]
name    = "doc-drift"
skill   = "update-docs"        # resolved against Skills/
every   = { commits = 25 }     # or { prs = 10 }, or both (fire on either)
min_gap = "24h"                # wall-clock floor, regardless of count
pool    = "maintenance"        # existing executor-pool routing
deliver = false                # silent unless it opens a PR
```

`ignore_authors` is deliberately absent: excluding the pack's own commits should be the
**default and not configurable**, because a deployment that gets it wrong self-triggers forever.
