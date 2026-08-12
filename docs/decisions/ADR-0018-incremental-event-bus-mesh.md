---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0018
open_items: false
---

# ADR-0018: Incremental Event-Bus Mesh (with checkpoints)

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0018 (`incremental-event-bus-mesh`)

## Context

The single enabler for the whole modularity vision — vault-optional, multiple
dispatchers/executors, cron, partial deploys, self-improvement-as-a-service — is that components
publish/subscribe events rather than calling each other directly. *How* we get there determines
whether the substrate ships at all.

## Decision

See body for full decision text.

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- `../future-work/ideas/archive/meshify.md`
- `docs/ideas/a2a-protocol-idea.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: The single enabler for the whole modularity vision — vault-optional, multiple
dispatchers/executors, cron, partial deploys, self-improvement-as-a-service — is that components
publish/subscribe events rather than calling each other directly. *How* we get there determines
whether the substrate ships at all.

**Decision**: Adopt [`meshify.md`](../future-work/ideas/archive/meshify.md)'s direction — components publish/subscribe
events rather than calling each other — but **incrementally**, NOT as a big-bang refactor. Wrap seams
behind an `EventBus` trait **as they are touched**; **new components are bus-native from day one**;
old ones migrate when next touched (the chat -> dispatcher wiring in roadmap Phase 1 is the first
seam). Safety (narrowing, zone checks, provenance stamping, magnitude gates) stays in the bus layer —
services only consume or produce events the bus has already validated.

**Guard against drift** with concrete "the mesh is real now" checkpoints tied to features, so the
substrate doesn't quietly stall:
- **Checkpoint #1 (Phase 1)** — the capability catalog is a **live, bus-queryable registry**, not
  static config (the same registry the TUI/WebUI query).
- **Checkpoint #2 (Phase 2)** — the **coding-agent is a bus service**; an MCP hot-reload re-registers
  in the catalog.
- **Checkpoint #3 (Phase 3)** — ? **done (2026-07-04)**: **cron and vault-watch are interchangeable
  event-sources** — a new `EventSource` trait (`liberado-common`) that `Daemon::run` fans into one
  channel; the existing vault-watch loop was refactored into its first conformer before cron (new
  `liberado-cron` crate, vault-agnostic) became the second, proven by a daemon integration test
  asserting both produce reactions over the same channel. A third source landed the same day,
  beyond the checkpoint's original wording but the same seam: an external webhook receiver
  (`POST /api/hooks/{name}`, `crates/server/src/hooks.rs`) — a *push*-style producer (an external
  caller decides when to fire) rather than cron/vault-watch's *pull*-style (each runs its own loop),
  enabled by a new `Daemon::event_sender()` accessor so a same-process external producer can inject
  an `Event` with no `EventSource` loop of its own. Its second half — a second, independently
  config-enableable dispatcher/executor **pool** — also landed the same day: `Daemon` holds
  `pools: HashMap<String, DaemonPool>` (an always-present `"default"` entry preserves every
  pre-existing call site unchanged); `EventPayload.pool`/`CronSchedule.pool`/`HookConfig.pool` route
  a trigger to a named pool (`topology.toml`'s `[[pools]]`, validated fail-fast against a declared,
  enabled entry); a pool's authority is just its own `policy.toml` grant (`[[grants]]` keyed by the
  pool's name, the same mechanism `"dispatcher"`/`"main-agent"` already used) — no new authority
  mechanism needed. A privilege-escalation-shaped gap surfaced mid-implementation and was closed
  before landing: `Proposal.pool` is now a signed field (set by the pool's own `Orchestrator`
  *before* signing, re-verified defensively in `execute_approved`), so an approved proposal always
  executes under the *same* pool's authority it was proposed under, never a broader one reached via
  routing drift. Proven by a dual-pool daemon integration test: two pools given identical decisions
  referencing the same MCP, one granted it and one not — the ungranted pool's dispatcher-level
  guard (not just the orchestrator) catches the gap and never reaches a real runtime. Deliberately
  **out of scope** (research-confirmed, see `docs/ideas/a2a-protocol-idea.md`): pools do not
  coordinate or communicate with each other — that's a different, harder, currently-unproven
  problem (internal peer-agent authority-sharing), not what this checkpoint built.

**Rationale**: The mesh is the single enabler for the modularity vision (vault-optional, multiple
dispatchers/executors, cron, partial deploys, self-improvement-as-a-service). A foundation-first
build risks months of plumbing with nothing shipped; incremental-with-checkpoints gets the substrate
as a **side effect of feature work** while the checkpoints keep it honest. The public HTTP/SSE API and
the TUI client never change during the migration.

**Status**: Decided (2026-06-26). Realized incrementally across roadmap Phases 1–3; checkpoint #3
(both its event-source half and its second-dispatcher-pool half) done 2026-07-04.
