# Liberado — Roadmap

**What is not done yet.** Forward-looking work only: open debt, known-broken things, and what to
build next. What *is* built is described in [`../architecture/overview.md`](../architecture/overview.md);
finished plans and closed audits are in [`archive/`](archive/README.md).

Before starting anything, read [`../architecture/failure-modes.md`](../architecture/failure-modes.md) —
five bug classes this codebase produces over and over, distilled from twelve audits. Every one of them
shipped with a green test suite.

## Open now — in priority order

The order is deliberate and strategic, not a to-do list: **automation daemon → chat → coding.** The
*why* — and why sequencing effort this way is what makes the full "replace all three" goal reachable
rather than abandoning it — is in [`../architecture/positioning.md`](../architecture/positioning.md).
Short version: get **one** thing over the daily-driver line, dogfood it hard (dogfooding is what has
found every real bug so far), and let the shared substrate it hardens carry the other two.

### Priority 1 — the autonomous life-OS daemon (replace OpenClaw / Hermes)

*Already in hand: TurboVault storage, the cron event-source substrate, the Telegram notifier, the
capability boundary.* What remains is the **interfacing loop** — an agent that works while you are
away, pings you, and lets you answer — plus maturing crons and MCP breadth.

| # | What | Why it matters |
|---|---|---|
| **W1** | **A goal-session view in the WebUI** (phone-first) | See and steer your autonomous agents. The WebUI has `chat.rs` and **no session view at all** — it satisfies none of [`session-surface-contract.md`](../architecture/session-surface-contract.md). Needs a rendered UI and your eyes. |
| **E5-b** | **Answer a session from your phone** | The OpenClaw/Hermes killer feature. A ping reaches you; a reply typed into Telegram goes nowhere (confirmed live) — Telegram is one flat chat with **no session multiplexing**. The fix is a **deep link into W1**, not a reply bridge. Needs `public_base_url` — **not before there is a page to link to**. |
| **C1** | **Crons that spawn real sessions** | **Delivery half landed 2026-07-18** (see below): scheduled briefings fire a background session and push its summary to Telegram, and the 3 OpenClaw briefings were retired onto Liberado. What remains is the *interactive* payoff — "run this every morning, **ask me if you're unsure**" (an AskHuman-capable cron via E7 profiles). Current dogfooding gap: the briefings come back `PartiallySucceeded` because `liberado-weather-mcp` and `liberado-caldav-mcp` fail on the real path — reliability work in flight (`goal.md`). |
| **M1** | **MCP breadth** | The author named MCP connections a P1 capability. Two concrete gaps: connection **pooling/reuse** (today a fresh connection per execution — TurboMCP's `SessionManager` is worth recycling per transport group), and **multi-server registry UX** (declare several stdio+HTTP servers from config; the `McpRegistry` machinery exists, the registration surface does not). |
| **T1** | **The live conformance suite** — [`live-conformance-suite.md`](live-conformance-suite.md) | **Reliability is not optional for a daemon meant to run your life unattended** — this belongs here, in P1, not in a coding tier. Every defect found on 2026-07-13/14 passed the test suite and died on first contact with a running daemon, and those checks exist only as commands typed by hand. Most need a real daemon but only a `MockProvider`, so the valuable tier is fast and CI-able. |

### Priority 2 — a lean chat surface (replace LibreChat)

Gated on the WebUI maturing past the P1 session view. The goal is a self-hosted, single-binary chat
that is *yours* and light enough to run — not to out-feature LibreChat.

| # | What | Why it matters |
|---|---|---|
| **CH1** | **WebUI chat maturity** | Beyond the session view: model/provider switching, a daily-usable surface. The provider layer is already agnostic (Decision 13); this is surface work. |
| **CH2** | **Chat history search (Tier 1)** — [`chat-search-plan.md`](chat-search-plan.md) | Lexical/ripgrep search over history, near-free given the deliberately greppable per-conversation JSONL layout. Later tiers (BM25/`tantivy`, vector) only if Tier 1 proves insufficient. |

### Priority 3 — coding, good enough and integrated (NOT replacing Claude Code / Grok / Kilo)

The bar here is *integration parity for the author's workflow*, not best-in-class. A coding session is
just another `Session` on the same daemon; that it is joinable, capability-scoped and waits for you is
the whole value, not raw coding skill.

| # | What | Why it matters |
|---|---|---|
| **E6-c(b)** | **Resume a session parked *mid-build*** | Intake resume shipped (E6-c(a)); the build loop cannot, because re-running redoes filesystem work. The workspace is a git repo, so a commit is the obvious suspend point. A design pass, not a line of code. |
| — | [`pr-dispatch-vtcode-no-write-finding.md`](pr-dispatch-vtcode-no-write-finding.md) | **Root cause still not found.** The one genuinely open bug carried over. |
| — | [`coder-eval-curriculum.md`](coder-eval-curriculum.md) | Measures coding quality on an instrument. Only matters once P1/P2 are not the bottleneck. |

### Cross-cutting — the enabler, and the move-on bar

- **Modularity and dedup are the load-bearing enabler**, not neatness: they are what let effort go to
  P1 without foreclosing P2 and P3 (see [`../architecture/positioning.md`](../architecture/positioning.md)
  and [`../architecture/modularity.md`](../architecture/modularity.md)). The dogfooding dividend lands
  on the **shared substrate** — kernel, session model, capability boundary, store — so the larger that
  shared share stays, the more P1's polish transfers to the others. Keep it large.
- **The move-on bar (set it consciously, don't drift):** leave P1 when you **daily-drive it without
  wincing** — not when it is polished. Sequencing's failure mode is gold-plating category 1 forever
  while chat and coding never get their turn; the dogfooding itself is the signal — when you reach for
  it without friction, that is the bell.

## Recently landed

Read [`../architecture/sessions.md`](../architecture/sessions.md) for the model itself; these are
just pointers to how it got here.

- **Cron → Telegram delivery, and the first OpenClaw cutover (2026-07-18).** A `cron:`-sourced
  session's summary is now delivered to Telegram (`daemon::maybe_deliver_cron_result`): a scheduled
  brief that used to store its output silently now reaches you. The three OpenClaw briefings
  (daily-planning, evening-debrief, weekly-review) were ported to `[[schedules]]`, enabled, and their
  OpenClaw originals disabled — Liberado now owns them. Same rebuild also shipped the Telegram
  **free-form chat surface** (typed replies answer a session; retires the send-only limitation).
  Delivery is smoke-verified live end-to-end. **The dogfood immediately paid off:** the real briefings
  first came back `PartiallySucceeded` because `liberado-weather-mcp` (geocoding rejected "City, State")
  and `liberado-caldav-mcp` `list_events` (relative-href "builder error" + datetime arg format) failed
  — both fixed and a real brief now returns `Succeeded` live. This is exactly the "get one thing over
  the daily-driver line and dogfood it hard" loop working as intended.
- **Cron briefs fold into the sticky Telegram conversation (2026-07-18).** A `deliver_cron` seam on the
  `Notifier` lets a chat-aware notifier (`server/src/cron_delivery.rs`) `append_note` each brief into
  the sticky Telegram chat session and **defer the push around your activity** — quiet-delay (default
  5 min idle) with a hard cap (default 45 min), so a reply to a brief carries it in context and a brief
  never barges into an active chat. Design + the deferred reply-to-threading follow-on:
  [`../ideas/cron-delivery-timing-idea.md`](../ideas/cron-delivery-timing-idea.md). **The sticky id now
  persists across restarts** (`server/src/sticky.rs`, `<data_dir>/telegram-sticky-session`): a restart
  no longer forces an implicit `/new` (restored id validated against the chat store, stale pointer
  discarded). Live-verified across a restart 2026-07-18.
- **The unified Session model (D7).** Everything is a `Session`; `goal: Option<_>` is the only
  difference between a chat and a run-to-terminal session. One store, one id space, one list.
  Session profiles + `Capability::AskHuman`; intake-first coding sessions; forking from any message.
  History: [`archive/session-focus-plan.md`](archive/session-focus-plan.md).
- **One execution engine (E1–E7).** `/spawn`, cron, webhooks and `delegate` all run on the one
  `GoalSessionHub` — the dispatcher/orchestrator pair is now the `dispatch` pack, so a daemon
  reaction is a session you can join, watch and cancel. `BackgroundRun` deleted.
  History: [`archive/one-execution-engine-plan.md`](archive/one-execution-engine-plan.md).
- **Sessions that wait for you (E5, E6).** A coding session asks mid-build, pings you when nobody is
  watching, waits for a profile-configured idle budget measured in hours, and folds your answer into
  the next attempt. Parked sessions survive a restart and — since E6-c(a) — **answering one resumes
  it**, with the pack rebuilding the negotiation from its own transcript.
- **F1 — `Capability::Write` is now enforced at the MCP boundary.** It never was: a profile granted
  `Read` and explicitly denied `Write` wrote to the vault, live. Declaring an MCP now means saying
  **what it touches**, and the daemon refuses to boot until you do.
- **V1 — the outcome vocabulary.** Investigating "five overlapping ways to say how it ended" found
  that **three of the five are not duplicates**: `Disposition` carries payloads, `VerdictStatus`'s
  `Error` (the check broke) is not `Fail` (the code is wrong), and `Outcome` is an *execution*, a level
  below a session. Each now documents why, so the next person to spot the "duplication" finds the
  reasoning rather than a tidy-up opportunity. The one real duplication — `TerminalKind` vs
  `SessionStatus`, converted by a hand-written match — is now a `From` impl plus its inverse, with
  `is_terminal()` *defined* as `terminal_kind().is_some()` so the two cannot disagree by construction.
- **S7-c — a contract that contradicts itself never reaches you.** And, after it killed a session by
  crying wolf, a machine check may now **defer to** a human but never **overrule** one.

**The lessons from all of the above are distilled in
[`../architecture/failure-modes.md`](../architecture/failure-modes.md).** That file is worth more
than this section.

## The phased roadmap (Phases 1–4) — done

All four phases are substantially complete. The sequence, the completion notes, and what each phase
deliberately deferred now live in
[`archive/phased-roadmap-2026-07.md`](archive/phased-roadmap-2026-07.md).

What is *built* is described where it belongs — in
[`../architecture/overview.md`](../architecture/overview.md), which is generated-adjacent and stays
current — rather than duplicated here as a growing list. This file is for **what is not done yet**.

## Nice to haves

### Independent safety rater (the "second opinion" model)

A separate, **cheap, completion-unbiased** model that rates an incoming goal for **danger** (and
optionally ambiguity), independent of the dispatcher/executor. Motivation: a model in a task-oriented
role carries a "get it done" pull that can subtly under-rate danger; an independent judge with no
stake in completing the task rates more conservatively.

Design constraints if/when built:
- **Downgrade-only** — like the deterministic guards, it can force `Clarify`/propose, never
  *authorize* action. It joins the "can only reduce autonomy" layer.
- **Actually independent** — ideally a *different model family* than the executor (a DeepSeek rating
  a DeepSeek shares failure modes; the value is in *decorrelated* judgments). We're provider-agnostic
  (Decision 13), so a different vendor for the "safety" role is a config change.
- **Danger > ambiguity** — danger is separable from routing and worth an independent signal;
  ambiguity requires understanding the goal to route it *anyway*, so a separate ambiguity rater is
  mostly redundant with the dispatcher.
- **Best placement** — a cheap **pre-dispatch triage**: rate danger first, short-circuit to
  `Clarify`/propose on a high score *before* paying for the dispatcher/executor. Improves safety and
  saves cost.

**Why it's deferred (not skipped):** the deterministic guards are strictly better for *enumerable*
danger (can't be talked out of it, free, exact) — that's the first line and the architecture's thesis.
The rater earns its place only on the *non-enumerable long tail*, and only if it **measurably** lifts
the safe-default / safety-regression metrics in `liberado-eval` (A/B "dispatcher alone" vs "rater +
dispatcher" on adversarial danger scenarios). Prove it on the instrument before adding it to the hot
path; don't add it on intuition.

### Other

*(MCP breadth and chat history search moved up into the priority tiers above — they are named
capabilities now, not someday-maybes.)*

- **Turn-budget "battery"** — captured as an idea, not scheduled:
  [`turn-budget-battery-idea.md`](../ideas/turn-budget-battery-idea.md). A graduated, agent-visible
  budget signal (3–4 states) plus a reserved wrap-up phase, so a cron that exhausts its turns degrades
  into a useful partial brief instead of a diagnostic dump — and a passively-adaptive per-schedule
  ceiling from session history. Motivated by the 2026-07-18 briefing that hit the budget wall
  mid-flail. P1 (unattended-cron reliability).
- **A2A (Agent2Agent) interop** — captured as an idea, not scheduled:
  [`a2a-protocol-idea.md`](../ideas/a2a-protocol-idea.md). The conversation-store seams
  (`author`, conversation lineage — Decision 17) and the mesh direction (Decision 18) already
  carry most of what this needs; the real gap is a new inbound protocol surface (AgentCard +
  Task lifecycle) and an outbound peer-delegation capability. Same category of work as
  vault-decoupling and cron (another event-source in, another external capability out).


