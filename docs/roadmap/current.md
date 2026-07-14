# Liberado — Roadmap

**What is not done yet.** Forward-looking work only: open debt, known-broken things, and what to
build next. What *is* built is described in [`../architecture/overview.md`](../architecture/overview.md);
finished plans and closed audits are in [`archive/`](archive/README.md).

Before starting anything, read [`../architecture/failure-modes.md`](../architecture/failure-modes.md) —
five bug classes this codebase produces over and over, distilled from twelve audits. Every one of them
shipped with a green test suite.

## Open now

| # | What | Why it matters |
|---|---|---|
| **W1** | **A goal-session view in the WebUI** (phone-first) | The WebUI has `chat.rs` and **no session view at all** — it satisfies none of [`session-surface-contract.md`](../architecture/session-surface-contract.md). This is the next real slice, and it needs eyes on a rendered UI. |
| **E5-b** | **Answer a session from your phone** | A ping reaches you; a reply typed into Telegram goes nowhere (confirmed live). Telegram is one flat chat with **no session multiplexing** — five sessions interleave and you cannot tell which asked what. So the fix is a **deep link into W1**, not a reply bridge. Needs `public_base_url` in config — **not before there is a page to link to**. |
| **E6-c(b)** | **Resume a session parked *mid-build*** | Intake resume shipped (E6-c(a)); the build loop cannot resume because re-running it would redo filesystem work. The workspace is already a git repo, so a commit is the obvious suspend point. Design pass, not a line of code. |
| **T1** | **The live conformance suite** — [`live-conformance-suite.md`](live-conformance-suite.md) | Every defect found on 2026-07-13/14 passed the test suite and died on first contact with a running daemon. Those checks exist only as commands typed by hand. **Most of them need a real daemon but only a `MockProvider`**, so the valuable tier is fast and CI-able — that is the insight that makes this tractable rather than a slow `#[ignore]`d graveyard. |
| — | [`pr-dispatch-vtcode-no-write-finding.md`](pr-dispatch-vtcode-no-write-finding.md) | **Root cause still not found.** The one genuinely open bug carried over. |

## Recently landed (2026-07-13/14)

Read [`../architecture/sessions.md`](../architecture/sessions.md) for the model itself; these are
just pointers to how it got here.

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

- **MCP connection pooling / reuse** — today a fresh connection per execution. TurboMCP's
  `SessionManager` (single-transport-type) is worth recycling per transport group for pooling +
  health + reconnection.
- **Multi-server MCP registry UX** — declare several servers (stdio `npx` + remote HTTP like
  deepwiki) from config; the machinery (`McpRegistry`, mixed `McpConnector`s) exists, the
  config/registration surface does not.
- **A2A (Agent2Agent) interop** — captured as an idea, not scheduled:
  [`a2a-protocol-idea.md`](../ideas/a2a-protocol-idea.md). The conversation-store seams
  (`author`, conversation lineage — Decision 17) and the mesh direction (Decision 18) already
  carry most of what this needs; the real gap is a new inbound protocol surface (AgentCard +
  Task lifecycle) and an outbound peer-delegation capability. Not before Phase 3 — same category
  of work as vault-decoupling and cron (another event-source in, another external capability
  out).
- **Chat history search** — design captured, not scheduled:
  [`chat-search-plan.md`](chat-search-plan.md). Three tiers (lexical/ripgrep, BM25/`tantivy`,
  vector/semantic), shipped in that order, stopping whenever the simpler tier proves sufficient —
  only Tier 1 has a clear "just build it" case today. `liberado-conversation-store`'s per-conversation
  JSONL layout was already designed to "stay greppable," so Tier 1 is a near-free fit, not a
  repurposing.


