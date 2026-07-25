# The session surface contract

**What any surface that shows sessions owes the user** — the TUI, the WebUI, a phone browser, a
future anything. Derived from what the TUI already does (it is a complete session client today) plus
[`../reference/api.md`](../reference/api.md), and stated once, here, so the WebUI does not have to be
reverse-engineered out of `crates/tui/`.

**Status**: 2026-07-14. The TUI satisfies most of this. The WebUI has **no session view at all** —
it has `chat.rs` and nothing else, so it satisfies none of it.

## What this is not

It is **not a layout spec**, and it is deliberately not derived from the TUI's *interaction* design.
This project already tested the theory that surfaces can share interaction abstractions: the
`ChatClient` trait in [`../roadmap/archive/tui-shared-code-extraction-plan.md`](../roadmap/archive/tui-shared-code-extraction-plan.md)
was proposed, never adopted, and **deleted** — "TUI and CLI's actual transport needs (blocking REPL
vs. non-blocking render loop) diverge too much for one shared trait to be worth forcing." What
survived was the narrow thing: shared wire types and one SSE decoder.

So a modal switcher and a hotkey are the *TUI's* answer to a problem, not *the* answer. A phone's
answer is a list you tap. This document specifies the **problem** and the **content**, and leaves the
answer to each surface.

## The job

> **Let a human make a good decision about work they did not watch.**

That is the whole requirement, and it is worth stating because it is the one Telegram fails. A
notification can say *"a session needs you"*. It cannot let you decide *well*: it is one flat chat,
so five sessions interleave into a single undifferentiated stream, and you cannot see which session
asked what, what you last told **that** session, or what it did with your answer. Answering a
question whose context you cannot reconstruct is a good way to give a confident wrong answer.

A session surface exists to make that decision *possible*. Everything below follows from it.

## The object

One `Session` (D7 — see [`sessions.md`](sessions.md)). A chat and a goal session are the same record;
`goal: Option<GoalSpec>` is the only difference, and it means "runs to a terminal status". Surfaces
**must not** model them as two things — the converged `GET /api/sessions` list exists precisely so a
switcher does not have to stitch two endpoints together.

## States, and what each means to a person

The status is a plain string on the wire (`SessionSummary.status`) so surfaces need not import the
kernel enum. What matters is that each one implies a **different thing the user can do**, and a
surface that renders them as interchangeable coloured badges has failed:

| Status | What it means | What the user can do |
|---|---|---|
| `pending` / `running` | Working. Nobody is needed. | Watch, cancel |
| *(awaiting input)* | **Stopped, holding a question for you.** Not a status — a flag (`awaiting_input`) plus an `awaiting_input` event carrying `{prompt, options}`. It can be true while `running`. | **Answer**, cancel |
| `parked` | Was awaiting you when the daemon restarted. The question it holds **is still visible**. It has **not** finished. | See what it wanted. **Not answerable yet** (E6-c) — `POST …/message` returns 409. Render as *"was waiting for you"*, **never** as dead or as a live prompt. |
| `succeeded` / `failed` / `cancelled` / `budget_exhausted` | Terminal. | Read, fork |

**`awaiting_input` is a flag, not a status, and this trips people up.** A session can be `running`
*and* awaiting you. The TUI models this correctly (`JoinedSession.awaiting: Option<AwaitingPrompt>`
alongside a separate `status`), and any surface that collapses the two will silently lose the ability
to show a question.

## Actions

Every one of these already exists server-side. **No new server work is needed to build a session
surface** — the TUI consumes all of them today (`crates/tui/src/api.rs`).

| Action | Endpoint | Legal when | Notes |
|---|---|---|---|
| List | `GET /api/sessions` | always | Chats *and* goal sessions, newest first |
| Watch | `GET /api/goals/{id}/stream` | always | SSE; catch-up history, then live. **Holding this stream open suppresses the out-of-band ping** (`live_subscriber_count`), which is correct — but it means a surface must not keep a stream open for a session the user is not actually looking at, or their phone will go quiet. |
| Snapshot | `GET /api/goals/{id}` | always | Record + full event history. Use this, not the stream, for a view the user opened cold. |
| Answer | `POST /api/goals/{id}/message` | awaiting | `202` ok · `403` the grant has no `AskHuman` — it may **never** be answered · `409` finished **or parked** · `404` unknown |
| Cancel | `POST /api/goals/{id}/cancel` | non-terminal | Cooperative |
| Fork | `POST /api/sessions/{id}/fork` | has a transcript | `{after_turn?, title?}` — branch from any point, keeping the original |
| Start | `POST /api/goals` | — | `{description, domain, profile?, max_idle_secs?, payload?}` |

**The three failure codes on `answer` mean genuinely different things and must not be rendered as one
error.** `403` is "never allowed" (an authority fact — the session's grant omits `AskHuman`); `409`
is "not now" (finished, or parked); `404` is "no such session". Collapsing them into "couldn't send"
tells the user nothing about whether to wait, retry, or give up.

## What must be visible to answer a question *sensibly*

This is the part a notification cannot do, and therefore the part that justifies the surface existing
at all. When a session is awaiting input, the user must be able to see, **without leaving the view**:

1. **The question** — `prompt`, and its `options` if any (they are suggestions, not a closed set;
   free text is always valid and is what the coder pack's "revise the contract" path depends on).
2. **Which session is asking** — its description/title. Non-negotiable when several are in flight.
3. **What it already tried.** The last turns and the recent events: the failing verifier, the
   summary, what the pack did before it got stuck. A coding session asks *"the build did not succeed:
   <summary>. How should I proceed?"* — answering that blind is guessing.
4. **What you last told it.** Human turns are in the transcript (`human_input` events are echoed
   back), so a returning user can reconstruct the thread. This is exactly what a flat chat cannot do.
5. **How long it will wait.** `max_idle_secs` — often hours. A user who does not know the session
   will still be there after dinner will answer badly, or not at all.

A surface that shows only (1) is a notification with extra steps.

## Listing, and why it is the hard part

Multiplexing **is** the requirement, not a nicety. The realistic state is several sessions in flight,
some background (a cron, a webhook reaction), one or two of them holding questions. The list must
therefore make immediately answerable:

- **Which of these need me?** (`awaiting_input`, and `parked` — the ones that needed you and were
  interrupted). This is the primary sort key of the whole product.
- Which are still working, which are done, which failed.
- Which I started (`foreground`) vs. which the daemon started on its own (`background` — `visibility`
  on the wire). A user should never be surprised by work they did not ask for.
- The session tree: `parent_session` is a real id, so a delegated/forked child is walkable back to
  its origin.

## Conformance today

| | TUI | WebUI |
|---|---|---|
| List sessions | ✅ `/session` browser + switcher | ❌ |
| Watch a session | ✅ | ❌ |
| See an awaiting prompt | ✅ (`AwaitingPrompt`, input box goes "hot") | ❌ |
| Answer | ✅ (`post_goal_message`) | ❌ |
| Distinguish 403 / 409-parked / 409-finished / 404 | ✅ *as of 2026-07-14* — writing this document is what found that it **did not**: `403` fell through to a generic `"server returned 403"`, and `409` was hardcoded to *"this session has finished"*, which is a **lie for a parked session**. The exact bug that had just been fixed in the API was still sitting in the client. | ❌ |
| Cancel | ✅ | ❌ |
| Fork from any message | ✅ (`f` at the cursor) | ❌ |
| Start a session | ✅ (`/spawn`) | ❌ |
| Render `parked` honestly | ⚠️ coloured in the switcher, and answering one now says so — but there is still **no affordance** telling you what to do about it | ❌ unknown status |

That row is the argument for writing this document down. A contract stated once is checkable; a
contract that lives implicitly in one surface's source cannot be checked, even by the surface that
holds it.

The WebUI is one entire feature-set behind, and this is the concrete reason **not** to grow the TUI
first as a way of "deriving the spec": every session feature added to the TUI widens this table
rather than narrowing it. The spec already fell out. This document *is* it.

## Known gaps this contract exposes

- **`parked` has no affordance anywhere.** The TUI colours it; nothing tells the user *"this was
  waiting for you and cannot be answered until it is resumed"*. The status was added by E6 and no
  surface learned what it means. (Same shape as the bug where the API told a parked session it "had
  already finished" — a state nobody taught the system to describe.)
- **The WebUI has no session view**, which is what blocks E5-b (the deep link into a session from the
  out-of-band ping). See [`../roadmap/archive/one-execution-engine-live-test.md`](../roadmap/archive/one-execution-engine-live-test.md) § E5-b.
- **`public_base_url` does not exist in config**, and deliberately should not until there is a page to
  link to — an unused config key is a lie about what the system can do.

## The phone case, stated once

The link in the ping opens on a phone. That is the *point* of it, and it is why the WebUI session
view should be designed phone-first rather than as a desktop page that reflows. The minimum useful
phone view is small and very well defined by everything above: **the question, who is asking, what it
tried, what I said last, and a text box.** Nothing else has to work on a 390px screen for the feature
to land.
