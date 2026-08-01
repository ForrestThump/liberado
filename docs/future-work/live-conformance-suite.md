# The live conformance suite

**Status**: Tier-1 **complete** (levels L1–L11); **Tier 3 open** (live, per-path, against the
deployed daemon — see below; added after three defects on 2026-07-28 that Tier 1 structurally could
not see) — **L1–L8 / L10 / L11** in
`crates/server/src/t1_conformance.rs` (in-process production-shaped goals surface, durable
`SessionStore`, `MockProvider`; L6 via `RiskGatedToolRuntime` + spy write tool; L3/L4 via
parked reopen + production `POST .../message` → resume; L7 via spy `SessionAlert` + real
`live_subscriber_count` dual-arm). **L9** lives on the shipped daemon reaction path:
`liberado-daemon` test `l9_cron_event_becomes_joinable_dispatched_session` (cron/webhook-class
event → `ReactionOutcome::Dispatched { session_id }` + joinable hub session). Tier 2
(model-in-the-loop) remains optional / `#[ignore]`d.
**Why**: [`../spec/architecture/failure-modes.md`](../spec/architecture/failure-modes.md) — the meta-lesson.

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

### Tier 3 — against the **deployed daemon**, one run per path, on a schedule

**Status**: open, and the highest-value unbuilt thing here (added 2026-07-28).
**Building it?** This section is the argument; the deliverables, safety envelope and per-path
assertion contract are in [`live-conformance-tier3-build-spec.md`](live-conformance-tier3-build-spec.md).

Tier 1 is in-process with a `MockProvider`. That is the right design and it is why it runs in CI —
but it means Tier 1 cannot see anything that only exists on a real deployment: the actual config on
the box, the real provider's wire format, an MCP that is up, the request body we genuinely send.

Three defects on 2026-07-28 landed in exactly that blind spot, and **all three were found by running,
none by review**:

| defect | why Tier 1 could not see it |
|---|---|
| `to_openai_request` discarded every caller's JSON schema, sending `json_object` instead | nothing asserted the *request body*; `MockProvider` accepts any shape |
| both morning crons dead for a day | no path runs cron→dispatch→execute against a live model |
| `POST /api/goals {"domain":"dispatch"}` started a powerless session | needs the real `policy.toml`, where the grant is `dispatcher` |

The schema one is the instructive case. Four callers wrote correct schemas; the boundary dropped all
four; the coding pack independently grew `extract_json_object` to cope, which **masked** it locally.
A correct-looking abstraction, used correctly, unplugged at the edge, for an unknown length of time.

#### What it is

One scripted run per **path**, against the deployed daemon, on a schedule (nightly is enough), each
asserting an outcome rather than a status code:

- **cron → dispatch → execute** — fire a schedule's real goal; assert it classified, ran, and
  delivered. This is the one that was broken.
- **chat turn** — a message through `/api/chat/stream`; assert tokens, and a persisted transcript.
- **hook** — `POST /api/hooks/{name}`; assert a joinable session.
- **spawn** — a profiled goal; assert it ran under the profile's grant, not the domain fallback.
- **delegate** — a chat turn that must delegate; assert a child session with the dispatcher's grant.

#### The rule that makes it worth having

**Assert the thing that would be wrong, not that nothing errored.** A cron returning `202` proved
nothing on the 28th — the session started and then failed every action. The assertion has to be
"a brief arrived", "the tool surface was these N tools", "the request carried a `json_schema`".

#### Cheap companion: seam tests

Independent of Tier 3 and worth doing first because it is nearly free — for every "we send X to the
provider" abstraction, one unit test inspecting the **built request body**. Two now exist
(`a_schema_that_constrains_shape_is_sent_as_json_schema`,
`a_shapeless_schema_falls_back_to_json_object`). That class needs a sweep: tools, temperature,
max_tokens, and whatever the next boundary carries. This is what would have caught the schema bug in
CI years earlier than a live run would.

#### Why this and not more unit tests

95k lines of Rust across 43 crates, and five distinct execution paths, none of which gets the volume
of live iteration a single-purpose coding agent's one loop does. Breadth is the project's goal, so the
answer is not less breadth — it is **proof per path**. A path nothing exercises end to end will break,
and you will find out from Telegram.


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

Tier 3 is now the open one, and the **seam tests** under it come first: they are unit-cheap, run in
CI, and would have caught the defect that cost a day of crons. Then the cron path, because it is the
one that has already failed silently. The remaining paths in whatever order they next surprise you.
