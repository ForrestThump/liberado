# mcp-forge backlog — what gets wired in as an independent project vs. built core

**Status**: Living backlog, not a committed schedule. Purpose: as capability gaps get identified
(this doc started from the doom-loop investigation and the MCP/A2A/hooks protocol discussion,
2026-07-04), sort each one into "wire in via `mcp-forge` as its own Liberado-branded repo" or
"build tightly coupled into the core crates" *before* building it, instead of defaulting to
whichever feels easier in the moment.

## The sorting test

Not "does it look like a tool call" (see the MCP/A2A/hooks conversation this doc grew out of —
protocol shape doesn't determine trust boundary). Two real questions:

1. **Authority**: does the *model* get to choose whether this happens, or must a deterministic
   layer be able to override/reroute regardless of what the model intended? If the latter
   (proposal downgrading, risk gating), it cannot be "just an optional MCP a prompt might skip" —
   it has to sit in the runtime path every capability call already goes through
   (`RiskGatedToolRuntime`), independent of whether the underlying capability is itself MCP-shaped.
2. **In-band vs. out-of-band**: does the effect feed back into *this* execution's own control flow
   (a normal tool call), or does it register/spawn something for a *different*, future execution
   context (a scheduled wake-up, an external agent handoff)? In-band and zero-external-consequence
   → core engine state, no need for a real connection/discovery round trip. Out-of-band → needs an
   activation contract (new conversation? resumed one? default capability posture?), which is a
   design question regardless of transport.

Neither test cares whether the wire format happens to be MCP's JSON-RPC shape — that's a free
implementation choice once the authority/effect questions are answered.

## Wire in via `mcp-forge` (independent, Liberado-branded repos)

Candidates that are genuine, capability-gated, external-world-touching tools — the model chooses
to call them or not, a `Consequence`/zone covers them, and `mcp-forge` (git URL → built binary,
`crates/mcp-forge/ARCHITECTURE.md`) plus a `topology.toml`/`mcp-sources.toml` entry is the right
amount of ceremony:

- **`riggers` / `liberado-pr-dispatch-mcp`** — already done, the existing precedent (Phase 2,
  `code-dispatch`, `consequence = reversible`). Every entry below should look like this when
  finished: its own repo, registered by name, human-authored `description`/`consequence` in
  `topology.toml`, never auto-merging anything.
- **A cron/scheduling MCP** (Phase 3's "cron as a bus listener," `docs/roadmap/current.md`) — the
  *inbound trigger* half (a cron tick firing) is an event source, not an MCP (see "Core" below),
  but a **task-management surface** for the agent to itself schedule/cancel future wake-ups
  (the equivalent of this session's own `ScheduleWakeup`) is genuinely model-invoked, capability-
  gated, external-effect (it changes what will happen later) — that part fits `mcp-forge` cleanly.
  **Built**: `liberado-wakeup-mcp` (its own repo,
  [github.com/ForrestThump/liberado-wakeup-mcp](https://github.com/ForrestThump/liberado-wakeup-mcp))
  is exactly this — `schedule_wakeup`/`cancel_wakeup`/`list_wakeups`, live-verified end-to-end
  against a real turbovault instance and the real daemon's webhook hook. Not yet registered as a
  `topology.toml` `[[mcps]]` entry (needs a human `consequence` rating — see `human-todo.md`).
- **A hooks/webhook-receiver MCP** — inbound HTTP webhooks are event sources (core, below), but a
  companion tool letting the agent *register a new webhook endpoint for itself* (rather than one
  hand-wired in `topology.toml`) would be genuinely capability-gated, external-effect work.
- **An A2A bridge, outbound direction** (`docs/ideas/a2a-protocol-idea.md`) — Liberado *delegating*
  a goal to a remote peer agent is model-invoked, capability-gated (Decision 8's subagent-narrowing
  logic, but for a strictly-less-trusted peer), external-effect. Note: A2A itself is a full
  protocol, not "an MCP" — per the saved conversation (`docs/ideas/mcp_acp_protocol_difference_conversation.md`),
  this should be its own sibling crate (e.g. `turbo-a2a`), not folded into `turbomcp` or shoehorned
  into the MCP transport. The *outbound-delegate* capability the dispatcher sees, though, can still
  be exposed to the model the same way any other capability is.
- **Future real-world MCPs already flagged as pending**: caldav, calorie-counter, weather (blocked
  on an upstream stdio fix per `docs/roadmap/current.md`'s Phase 1 section) — same pattern as
  everything else already `mcp-forge`-managed.

## Core, tightly coupled — not `mcp-forge` material

Everything here fails the sorting test above for a specific, named reason, not just "seems more
central":

- **Proposal downgrading (Decision 11)** — `RiskGatedToolRuntime`. Fails test 1: if a human-in-
  the-loop downgrade were an optional tool the model could choose to route through or skip, the
  whole safety guarantee evaporates. Already built correctly (interception, not an offered choice).
- **`submit_report`** — already a synthetic, engine-injected tool (`SUBMIT_REPORT_TOOL` in
  `crates/executor/src/lib.rs`), not a capability grant. Fails test 2: its whole purpose is
  terminating *this* loop and handing back a typed result — pure in-band control flow, zero
  external consequence.
- **The doom-loop guard** (`is_doom_loop`/`detect_short_cycle`/tool removal, this session's work)
  — same shape as `submit_report`: it's the executor watching its own `call_history` and adjusting
  its own next turn. Nothing about it is a capability an agent invokes.
- **A task/scratchpad-tracking mechanism**, if built (deferred per the doom-loop finding doc's
  "what works best" research, `docs/ideas/doomloop_research.md`'s "external working memory" point)
  — fails test 2 the same way `call_history` does: it's bookkeeping about *this* execution's own
  progress, tightly coupled to the loop that produces it, zero external consequence. Could
  technically be built as an MCP (plenty of real systems do this) but the connection/discovery
  overhead buys nothing here.
- **Cron/hooks, the *inbound* trigger side** — an event firing and re-activating a dormant
  daemon/dispatch loop is not "a tool call an active loop makes," it's the thing that starts a loop.
  Fits the `event_source` trait direction already in `docs/architecture/overview.md`
  ("MCPs vs hooks" — hooks are event sources that *push* into the daemon, not tools the agent
  *calls*) and Phase 3's plan (cron and vault-watch as interchangeable event sources). The
  standardized-envelope gap this needs (see the MCP/A2A/hooks conversation) is closer to
  CloudEvents' shape than to anything MCP- or A2A-specific — worth deciding whether to adopt
  CloudEvents' envelope directly (external-standard credibility, same reasoning that favors reusing
  A2A's real spec over inventing one) or keep `liberado_common::Event` bespoke.
- **An A2A bridge, inbound direction** — an external peer's `Task` arriving is an event source
  triggering Liberado (same shape as a hook), not Liberado calling out. Per
  `a2a-protocol-idea.md`'s still-open question: does an inbound `Task` map to a new conversation
  (parent-linked, via the existing `author`/lineage seams in Decision 17) — undecided, but either
  way it's the daemon's event-intake side, not an outbound MCP-forge project.

## Open questions

- Whether the task-tracking mechanism gets built at all is still gated on evidence (per the
  doom-loop finding doc) — not scheduled.
- Whether `liberado_common::Event` should be reshaped toward CloudEvents' envelope, or stay
  bespoke, is undecided — matters most once hooks/cron/A2A-inbound all need to share one shape.
- Cron's own phased placement is already decided (Phase 3, `docs/roadmap/current.md`) — this doc
  doesn't move that up, just clarifies which *half* of it (inbound trigger vs. agent-scheduled
  wake-up) is core vs. `mcp-forge` material once it's picked up.
- A2A is explicitly "not before Phase 3" per `a2a-protocol-idea.md` — this doc doesn't change that
  either, just pre-sorts the inbound/outbound split for whenever it is picked up.

## Companion to

- `docs/ideas/a2a-protocol-idea.md` — the A2A interop design, seams already in place, open questions.
- `docs/ideas/mcp_acp_protocol_difference_conversation.md` — the MCP/ACP/A2A protocol comparison
  this backlog's sorting test is grounded in.
- `docs/ideas/doomloop_research.md` — the "external working memory" research point behind the
  task-tracking entry above.
- `docs/roadmap/current.md` — Phase 3 (cron, vault-decoupling) and the general phased sequence.
- `crates/mcp-forge/ARCHITECTURE.md` — the actual mechanism ("wire in via mcp-forge" means this).
- `docs/architecture/overview.md` — "MCPs vs hooks," the existing decided distinction this backlog
  extends rather than replaces.
