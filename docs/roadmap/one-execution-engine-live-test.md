# Live test: a session that works until it needs you

**Status**: sketch, 2026-07-14. Not yet run.
**Tests**: [`one-execution-engine-plan.md`](one-execution-engine-plan.md) E1–E7, as shipped in `671bde9`.

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

**E5-b (proposed)**: point the existing `ApprovalBot` reply machinery at
`POST /api/goals/{id}/message`, keyed by the session id already in the alert text. The plan called this
"smaller than it sounds" and that still looks right — the long-poll, the typed reply, and the routing all
exist. But it is not built, and until it is, the hours-long idle budget is a promise the *daemon* keeps and
the *product* does not.

Do the live run first: there is no point wiring a reply path to a feature that has not yet been shown to
fire correctly against a real model.
