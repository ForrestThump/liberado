# The live conformance suite

**Status**: planned, 2026-07-14. Not built.
**Why**: [`../architecture/failure-modes.md`](../architecture/failure-modes.md) — the meta-lesson.

## The case

Every defect found on 2026-07-13/14 **passed the test suite and died on first contact with a running
daemon**: the ask seam wired to the wrong path, the deadlocked progress guard, the `Read`-only profile
that wrote to the vault, the API telling a parked session it had finished, a `cancel` that was a no-op.
1,191 unit tests, none of them noticed.

Those checks exist today only as **commands I typed by hand**. Nothing stops the next change from
silently breaking any of them. That is the gap: not more unit tests, but a suite that exercises the
thing the user actually runs.

## The insight that makes this tractable

The instinct is "we need a live model, so this must be a slow, flaky, `#[ignore]`d test." **That is
mostly wrong**, and getting it wrong would produce a suite too expensive to run and therefore never run.

Look at what actually broke: the ask seam on the wrong path, `Parked` unanswerable, `Write` unenforced,
`cancel` a no-op, the ping suppressed by a stale subscriber. **Every one is plumbing, not model
behaviour.** They need a *real daemon* — a real hub, a real store on disk, a real HTTP surface, real
restarts — but a **`MockProvider`** is a perfectly good model for all of them.

So: **two tiers, and the cheap tier carries most of the value.**

### Tier 1 — daemon-level, `MockProvider`, runs in CI (the important one)

A real `liberado-server` on a temp port, real JSONL store in a temp dir, scripted provider. Fast,
deterministic, no API key, no network. This is where the bugs were.

| # | Check | Guards |
|---|---|---|
| L1 | Spawn → pack asks → answer over `POST /message` → pack **retries with the answer** → terminal | E5. Assert the **backend saw the answer**, not that an event said so |
| L2 | Kill and reopen the store mid-question → session is `Parked`, `awaiting_input` intact, question still visible | E6 |
| L3 | Answer a parked session → it **resumes** and the pack sees its prior turns | E6-c(a) |
| L4 | A pack that has started building refuses to resume; the session stays parked and says so | the irreversibility line |
| L5 | Grant without `AskHuman` → never prompts, `POST /message` → **403** (not 409) | S6 |
| L6 | Grant with `Read` and no `Write` → the MCP write is **refused**; with `Write` it **succeeds** | F1 — *both arms, or it proves nothing* |
| L7 | Session with a live SSE subscriber → **no** alert; without → alert fires | E5's ping, with its positive control |
| L8 | `cancel` on a long-running pack → reaches `Cancelled` | the no-op cancel mutation testing found |
| L9 | A cron/webhook event becomes a **joinable** `dispatch` session (`Dispatched { session_id }`) | E3 |
| L10 | Fork at turn *n* → the fork holds the prefix; continuing the original does not move the fork | copy semantics |

### Tier 2 — model-in-the-loop, `#[ignore]`d, run deliberately

Needs a provider key. Slow, non-deterministic, and **cannot be a gate** — but it is the only thing that
tests whether the *model* can actually drive the machinery.

- The **shibboleth run**: a coding session whose gate requires a token that exists nowhere in the
  workspace, the goal, or the verifier output — only in the human's head, as a SHA-256 preimage the
  model cannot invert. Answer it; assert `TOKEN.md` holds the exact secret. Recipe:
  [`archive/one-execution-engine-live-test.md`](archive/one-execution-engine-live-test.md).
- Intake reaches a coherent contract without the coherence checker burning its budget (the S7-c
  regression: three false contradictions killed a session having never asked the human anything).

## Design rules, learned the hard way

**Assert on ground truth, not on what the system says about itself.** The E5 bug emitted
`retrying once with human guidance` and retried nothing — every event, every status, every log line
said it worked. The only assertion that caught it looked at the **backend's second request**. In the
live run, the only assertion that mattered was `cat TOKEN.md`, not `status: succeeded`.

**Every refusal test needs its positive control.** A gate that refuses *everything* passes every
"must be refused" assertion and looks like perfect security. L6 and L7 are each two arms for this
reason; without the second arm they are worse than nothing, because they retire the worry.

**Isolate ruthlessly.** `LIBERADO_CONFIG_DIR` + `LIBERADO_DATA_DIR` into temp dirs. The live runs that
found F1 pointed TurboVault at a **throwaway vault** precisely because a guard failure means a stray
write into the real one — and the guard did fail. A conformance suite that can damage the user's vault
will get switched off.

**A check that cannot fail is not a check.** Before landing any of these, break the thing it covers and
watch it fail (§1 of `failure-modes.md`). L8 exists because a no-op `cancel` passed everything.

## What this is not

Not an end-to-end coding benchmark — that is [`coder-eval-curriculum.md`](coder-eval-curriculum.md),
which asks *"is the model any good?"*. This asks *"does the machinery work?"*, and the two fail for
completely different reasons. Keep them apart, or a model regression will look like a plumbing bug and
be debugged as one.

## Sequencing

Tier 1 first, and probably L5/L6/L8 first within it — they are the cheapest and they guard the
security-relevant behaviour. Tier 2 only after Tier 1 is green, because a flaky model-in-the-loop test
sitting on top of unverified plumbing tells you nothing you can act on.
