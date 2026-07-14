# Live test: a session that works until it needs you

**Status**: **RUN, 2026-07-14 — passed on the third attempt, after fixing two defects it found** (`1de63bc`).
**Tests**: [`one-execution-engine-plan.md`](one-execution-engine-plan.md) E1–E7, as shipped in `671bde9`.

## Result

`TOKEN.md` ended up holding `ORCHID-7Q-KESTREL-VELLUM-42` — a string that existed nowhere in the
workspace, the goal, the context, or any verifier's output, only in the human's head and as a SHA-256
preimage the model cannot invert. It could not have arrived there by any route except the human's reply
travelling into the backend's next attempt. The session asked, waited, was answered over the API,
retried, and went green. The out-of-band ping fired (nobody was watching) and reached a phone.

**Everything the unit tests asserted was true, and the feature still did not work.** Two defects, both
invisible to 1,148 passing tests:

1. **The ask seam was on the wrong path.** `CoderError::NoChanges` is an `Err`, and the ask only ran on
   the `Ok` path — so a build that *finished but failed its gates* asked, while a build that got
   genuinely **stuck** could not. The more stuck the pack got, the less able it was to ask for help.
   The first run built a working CLI, hit a gate it had no way to satisfy, and died silently at exactly
   the moment it should have spoken. Missed because `ScriptedBackend` returns `Ok(Failed)` — **the
   double made the shape I expected, not the shape reality produces.** Third time in this audit.

2. **The progress guard's remedy was the thing it blocked.** Once latched it returned `Fatal` for every
   call — including `write_file` and `submit_report`, the two escapes its own message demands. And
   `observe()` runs *after* the tool executes, so a `write_file` that succeeded on disk was reported
   back as a failure and the model "retried" an edit it had already made.

## The third finding: an incoherent contract executes faithfully

Run 2 failed for a reason that is **not a bug** and is more interesting than the two that were.

Asked to record "you cannot guess the token, you must ask me", the intake model wrote an out-of-scope
clause: **"Modifying TOKEN.md or guessing the release token."** The frozen contract therefore demanded
a gate that only `TOKEN.md` could satisfy while forbidding the coder from writing `TOKEN.md`. The coder
obeyed the contract and failed. That is *correct* behaviour — the whole point of freezing is that the
contract is authoritative and the worker cannot argue with it.

Which is exactly why it is dangerous. **The contract is the one artifact in the system with authority
over the work, and nothing checks it against itself.** See "S7-c" below.

## What only a live run can prove

The unit tests already pin the *mechanism*: a failed build asks, and the human's answer arrives in the
backend's next attempt as `prior_feedback` (`a_failed_build_asks_the_human_and_retries_with_their_answer`).
That test exists precisely because the first cut of E5 faked it — it asked, took the answer, announced a
retry, and retried nothing.

So the live run is not for the mechanism. It is for the four things a test double cannot fake:

1. A **real coding model**, against a **real workspace**, actually reaching the ask seam — i.e. failing a
   verifier in a way its own internal repair budget cannot fix. (If the repair loop just fixes it, the
   ask never fires, and E5 is dead code in practice.)
2. The **notification** firing only when nobody is watching — the `receiver_count() == 0` branch, which
   no unit test observes end-to-end.
3. The **hours-long wait** actually holding: the session still alive and answerable after a real work day.
4. The **honest limits** behaving as documented: a cron cannot ask, one ask means one retry, and a parked
   session is visible but not answerable.

## The design problem: making the answer *necessary*

The trap this whole audit keeps falling into is a test that would pass either way. A live run has a
sharper version of it:

> The build fails. We answer. It retries. It succeeds. **Did our answer cause that, or did the retry just
> succeed on its own?**

This is not hypothetical. The coding loop already re-runs with **verifier feedback** (`repair_feedback()`)
inside its own attempt budget. So a plain "you forgot to create `NOTES.md`" failure is one the loop can
diagnose and fix *without any human*. If we build the test that way, a green run proves nothing — the
guidance could have been dropped on the floor and the outcome would be identical.

So the failure has to be one where **the verifier feedback cannot contain the answer.**

### The shibboleth

Seed the workspace, before the session starts, with a test that checks a value the model cannot derive:

```rust
// tests/token.rs — committed into the workspace before the goal is dispatched
#[test]
fn release_token_matches() {
    let token = std::fs::read_to_string("TOKEN.md").expect("TOKEN.md must exist");
    // The expected value appears NOWHERE in the repo, the goal text, or the failure output.
    let digest = sha256_hex(token.trim());
    assert_eq!(digest, "9f2b…", "token mismatch");
}
```

The frozen contract's verifier is `cargo test`. On the first attempt the model creates `TOKEN.md` with
*something* — it has no way to know what — and the test fails with `token mismatch`. The failure output
leaks nothing: not the token, not the hash preimage. The internal repair loop cannot solve it, burns its
budget, and the build fails. **That is what drives it into the ask seam.**

Then we answer: `the token is ORCHID-7Q`. If the session goes green, the human's words provably reached
the backend, because nothing else in the system knew them. If the plumbing is broken, it fails again with
exactly the same `token mismatch` — a clean, unambiguous negative.

This is artificial on purpose. The realistic version of "the model needs a decision only you can make" is
genuinely hard to construct deterministically; the shibboleth reproduces its *information structure* (an
answer that exists only in the human's head) without depending on model judgment. Run the realistic
version too (below, Test A′), but do not let it be the thing the pass/fail hangs on.

## Setup

- A provider key (`OPENROUTER_API_KEY` / DeepSeek), since the coding pack needs a real model.
- Telegram env for the notifier (`TELEGRAM_BOT_TOKEN`, chat id).
- A scratch workspace that is **its own git repo** (the pack's changed-file gate is repo-scoped), seeded
  with the shibboleth test.
- A profile in `config/topology.toml`, restored afterwards:

```toml
[[session_profiles]]
name         = "coding-interactive"
domain       = "coding"
capabilities = ["AskHuman", "..."]   # plus whatever the coding pack needs
max_idle_secs = 21600                # 6h — the point is answering after work

[session_profiles.overrides]
max_mid_run_asks = 1
```

## Test A — the headline

1. Start the daemon. Confirm `goal session packs: life + coding + dispatch` and
   `session alerts: telegram notifier attached`.
2. `POST /api/goals` with `profile: "coding-interactive"`, `payload.workspace_root` = the seeded repo.
3. Run intake, freeze a contract whose verifier is `cargo test`.
4. **Close every stream.** No TUI, no browser tab, no `curl` on `/stream` — otherwise `receiver_count`
   is not zero and the ping is (correctly) suppressed.
5. Build runs, `token mismatch`, repair budget burns, the pack asks.
6. **Expect a Telegram message** naming the session and the prompt.
7. **Walk away.** Come back hours later — well past any old default, well inside the 6h budget.
8. Answer: `POST /api/goals/{id}/message` with the token.
9. **Expect**: a second attempt (a second `role_started` for `coder` in the event stream), `TOKEN.md`
   written with the right value, `cargo test` green, session `Succeeded`.

**Read the transcript, not just the outcome.** The turns should show: the goal, the intake Q&A, the
question the pack asked, your answer, and the outcome. If the answer is in the transcript but the session
failed with `token mismatch` again, that is the *exact* regression the unit test was written for, and it
means the wiring is broken somewhere the double could not see.

### Test A′ — the realistic variant

Same shape, but the failure is a genuine design ambiguity (a storage choice the contract does not settle),
and the answer is a real decision. Grade this one on *judgment* — did it ask a question worth asking, at a
sensible moment? — not on pass/fail. This is the test of whether the feature is actually pleasant to live
with, which is the thing that decides if you use it.

## Negative controls — each pins one claim

| # | Setup | Expect | Pins |
|---|---|---|---|
| B | Same session, but keep it **open in the TUI** | Ask fires, **no Telegram ping** | The ping is driven by `receiver_count() == 0`, not by "a notifier exists" |
| C | Same goal, profile **without `AskHuman`** (a cron) | **No ask at all**; build fails cleanly and terminally | Interactivity is a capability, not a mode — a cron does not stall on a question nobody will answer |
| D | Model that never satisfies the verifier; answer the one question | **Exactly one** ask, then a terminal failure | `max_mid_run_asks` binds — a stuck pack cannot interrogate you |
| E | Kill the daemon while it is awaiting; restart | Session shows **`Parked`**, `awaiting_input` true, the question still visible; `POST …/message` **fails** | E6 as documented — visible and honest, *not* answerable (E6-c) |
| F | Profile granting only `Read`, on a pool that allows writes | The write is **refused** | E1 — a per-run grant genuinely narrows the pool ceiling |

Control **B** is the one I would most expect to fail, and it is the cheapest to get wrong: any lingering
SSE subscriber (a stale browser tab, a TUI in another terminal) silently suppresses the ping, and the
symptom looks identical to a broken notifier.

## The gap this surfaced

**The Telegram path is send-only.** `NotifySessionAlert` pings you and then tells you to *"Answer in the
TUI or via `POST /api/goals/{id}/message`"*. `telegram-approvals::ApprovalBot` long-polls `getUpdates` and
handles typed `force_reply` replies — but it only knows about **proposals**; it has no session awareness.

So the story the feature is *for* — "I'm at work, I get the ping, I answer from my phone" — does not close.
You get the ping on your phone and then need a machine to reply. Step 8 above is a `curl`, which is a
usable test but not a usable life.

Confirmed live: the human replied "accept" **in Telegram** and it went nowhere. The session sat waiting.

---

# Next: the two things this run put on the roadmap

## S7-c — Contract coherence: lint the one artifact with authority

**The problem.** `validate_draft` checks each verifier is *well-formed in isolation* (program non-empty,
paths non-empty). Nothing checks the draft **against itself**. Freeze then stamps a `content_hash` and
makes it binding, so an incoherent contract is not a soft error that gets muddled through — it is a
**durably authoritative** instruction to do something impossible, and the worker will faithfully obey it
into the ground. The human reviewing the draft is the only line of defence, and they are reading a long
prose block, possibly at 3am, on a phone.

Four failure classes seen in **three runs of one test** — this is not a rare event:

| Class | Seen | Why the human misses it |
|---|---|---|
| **Out-of-scope forbids what a verifier requires** | *"Out of scope: modifying TOKEN.md"* + a verifier only `TOKEN.md` can satisfy | The two lines are 15 lines apart in the prompt, and each is individually reasonable |
| **Unsatisfiable path verifier** | `paths_exist: target/release/todo` — crate is `todo-cli`, and on Windows it is `.exe`. Could *never* pass | Looks like diligence, not a landmine |
| **`verify_profile` silently re-adds verifiers** | Model said it dropped clippy/fmt; `expand_verify_profile_into` put them straight back. Its prose and its verifier list **disagreed** | The human reads the prose |
| **A false `assumed_default` stated as fact** | *"The release token is stored in TOKEN.md"* — it was not, and the model had no way to know | Assumptions are exactly what a skimming reader skips |

**The shape of the fix.** A deterministic check (no model) in `coder-core`, at the existing
`validate_draft` seam, in two tiers:

- **Contradiction → refuse to freeze**, hand back to intake with the reason. The unambiguous case: an
  `out_of_scope` line naming a path/file that a verifier requires the worker to produce or modify.
  Cheap to detect — extract the paths a verifier touches, look for them in the scope prose.
- **Warning → surface *in the freeze prompt*, above the fold.** Path verifiers that nothing in the plan
  could plausibly produce; verifiers added by profile expansion (label them: *"3 verifiers from you,
  3 added by `verify_profile = rust-strict`"* — that alone kills the disagree-with-its-own-prose case);
  success criteria no verifier covers.

The principle: **the artifact with authority deserves a linter, not just an eyeball.** We built the
freeze step so a model could not weaken the human's gates; we did not consider that the *human* would
be handed gates that contradict themselves.

## E5-b — Answering from your phone

Today the alert says *"Answer in the TUI or via `POST /api/goals/{id}/message`"*, which on a phone means
nothing. Two designs, and the second is the one worth having:

**(a) A Telegram reply bridge.** Point the existing `ApprovalBot` long-poll/`force_reply` machinery at
`POST /api/goals/{id}/message`, keyed by the session id already in the alert. Low friction — reply in
place, no context switch. But it is a keyhole: you answer a question without seeing the transcript,
the diff, or what the pack actually tried.

**(b) A deep link into the WebUI.** The alert carries a URL to the session on the homelab instance; you
tap it and answer in a real view that shows the question *and its context*. Strictly better, and it
subsumes (a) — but it needs two things that do not exist yet:

- **A goal-session view in the WebUI.** There is `chat.rs` and no session view at all. Sessions are only
  browsable in the TUI today. This is the real work, and it belongs with the WebUI maturity effort.
- **A public base URL in config.** The notifier composes the link, so it must know the instance's
  externally-reachable address (`topology.public_base_url` or similar). Trivial, but *do not add it until
  there is a page to link to* — an unused config key is a lie about what the system can do.

**Recommendation**: build (b), and treat (a) as a stopgap only if the WebUI view is far off. Until either
lands, the hours-long idle budget is a promise the **daemon** keeps and the **product** does not — the
session will wait all day for you, and you will have no way to answer it from where you actually are.

The alert text should get the link the moment (b) exists; the one-line composition site is
`NotifySessionAlert::session_needs_you` in `crates/server/src/lib.rs`.
