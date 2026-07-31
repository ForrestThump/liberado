# Idea: a turn-budget "battery" — a soft, agent-visible budget signal

**Status:** idea, captured 2026-07-18. Not scheduled. Related: [`doomloop_research.md`](archive/doomloop_research.md)
(the retry-loop failure this mitigates), and the P1 automation-daemon reliability story in
[`../roadmap/current.md`](../../roadmap.md) (C1 — crons that run your life unattended).

## The problem this is for

A cron-fired session runs on a turn budget. When it exhausts the budget mid-work, the current
behaviour is a hard wall: the session terminates and the summary is whatever the executor can say
about *why it stopped*, not about the task. A real incident (2026-07-18, the first live daily-planning
brief): weather and calendar MCPs were failing, the model burned all its turns retrying them, and the
message delivered to Telegram was literally

> Execution exceeded its 6-turn budget before filing a report. Calls made: …

— a diagnostic dump, not a brief. Two separate things were wrong: the tools were broken (fixed
separately), **and** the budget mechanism gave the model no chance to salvage a partial answer from
what it had already gathered.

## The idea

Two coupled changes, both cheap, both building on the executor's existing budget + its existing
"one-time recovery top-up after tool removal" seam.

### 1. A reserved wrap-up phase (the mechanism)

Split the budget into a **work phase** and a small **reserved wrap-up phase**. When the work phase is
exhausted, don't terminate — force a transition: *tools off, "write the best report you can from what
you already have."* For information-gathering crons (most of them) this turns "budget exceeded, here's
garbage" into "here's a real brief, minus the one source I couldn't reach." The honest-status tagging
stays (a wrapped-up partial is still `PartiallySucceeded`, not a faked success) — it just carries
useful content instead of a stack trace.

### 2. A "battery" the agent can see (the signal)

Surface the remaining budget to the model as a **graduated soft signal — like a battery icon with 3–4
states**, not a raw integer:

- `🔋 full` — plenty of budget, work normally.
- `🔋 half` — past the midpoint, prefer to finish gathering.
- `🪫 low` — converge now; stop opening new threads.
- `🔌 wrap-up` — final phase, tools off: produce the report from what you have.

Why graduated, not a raw number:
- A hard "3 turns left" invites two failure modes — premature "I'd better wrap up" when there was
  plenty of room, and the model simply miscounting. A coarse state is robust to both.
- Models plan well against *shape* (in the incident the model correctly parallelised five tool calls
  in turn one). A battery state is enough shape to prioritise against without being brittle.
- The `wrap-up` state is just #1's reserved phase made *visible* — the model enters wrap-up knowingly
  rather than being silently truncated. #1 and #2 are really one feature: a phased budget the model
  can see.

## The load-bearing guardrail

**Budget tuning must never become a way to paper over broken tools.** In the incident, no budget would
have helped — the tools were down, and a bigger budget just buys more retries of a broken call. So any
budget logic (especially the adaptive ceiling below) must distinguish **"ran out doing useful work"**
from **"ran out retrying a failing tool."** A budget that quietly grows to absorb a retry loop is
worse than one that fails fast and says "weather-mcp is down." Count only *productive* turns toward any
estimate; treat a run dominated by repeated identical tool failures as a tool alert, not a
budget-too-small signal.

## Follow-on: a passively-adaptive per-schedule ceiling

Crons are recurring, and **every run already records its turn count in the session store** — so the
turn-usage distribution is free. Set each schedule's budget from a rolling high percentile (P90-ish,
not the mean — a busy day with more calendar events legitimately needs more turns) plus a small pad,
under a hard ceiling so a pathological run can't ratchet it upward. This is strictly better than the
tempting "deliberately shrink the budget until it fails, then add a turn or two" search: probing-to-
failure is expensive (every probe is a real model run) and non-stationary (tool latency and model
behaviour drift). Passive observation costs nothing and self-corrects. Gate it on the guardrail above
(only productive turns count).

## Why it's worth doing (and why it's deferred)

It's the unattended-reliability story for P1: a scheduled brief nobody is watching should degrade into
a useful partial, not a dump, and should right-size its own budget over time without hand-tuning.
Build order if picked up: **#1 + #2 first** (one small feature, fixes the visible "garbage on
exhaustion" UX), then the adaptive ceiling once the productive-vs-retry discriminator exists. Deferred
only because tool *correctness* (weather/caldav) was the higher-leverage fix for the same symptom
first; this is what makes the exhaustion case graceful once tools are reliable.
