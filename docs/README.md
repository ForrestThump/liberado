# Liberado documentation

**Single source of truth** for humans and agents. Liberado is a Rust-native **agentic orchestration** system: one daemon, capability-bounded tools (MCP), domain packs (life-ops, coding), and thin surfaces (TUI, WebUI, CLI, Telegram).

If you are an agent: start at [Getting started](getting-started/quickstart.md) → [Architecture overview](architecture/overview.md) → [Roadmap (current)](roadmap/current.md) → [Handoff / ops](handoff.md). Read [Failure modes](architecture/failure-modes.md) before changing safety or tests.

---

## Navigation map

| Area | Path | What lives here |
|------|------|-----------------|
| **Getting started** | [getting-started/](getting-started/quickstart.md) | First boot, local run |
| **Architecture** | [architecture/](architecture/README.md) | How the system works *now* (contracts, sessions, modularity) |
| **Reference** | [reference/](reference/README.md) | HTTP/SSE API, generated crate map |
| **Roadmap** | [roadmap/current.md](roadmap/current.md) | **What is open next** (thin living surface) |
| **Specs** | [specs/](specs/README.md) | Frozen decisions and detailed design specs |
| **Handoff / ops** | [handoff.md](handoff.md) | Homelab, Telegram, live dogfood state |
| **Contributing** | [contributing/](contributing/agents.md) | Build/run, agent workflow |
| **Ideas** | [ideas/](ideas/README.md) | Forward brainstorms (not commitments) |
| **Research** | [research/](research/README.md) | Dated analyses (not the roadmap) |
| **Archive** | [roadmap/archive/](roadmap/archive/README.md), [ideas/archive/](ideas/archive/README.md), [research/archive/](research/archive/README.md) | Historical only — do not treat as current truth |

Open product/design ambiguities: [design_questions_for_the_user.md](design_questions_for_the_user.md).

---

## Cold-start (humans & agents)

1. [Quickstart](getting-started/quickstart.md)
2. [Architecture overview](architecture/overview.md) — pillars, daemon-first loop, safety
3. [Sessions](architecture/sessions.md) — everything is a `Session` (D7)
4. [Contracts](architecture/contracts.md) — narrow waists / frozen seams
5. [Failure modes](architecture/failure-modes.md) — five recurring bug classes
6. [Roadmap (current)](roadmap/current.md) — what to build next
7. [Handoff](handoff.md) — what is live on the homelab today

Per-crate detail: generated [crate map](reference/crate-map.md) + each crate’s `crates/*/ARCHITECTURE.md`.

---

## Product frame

Liberado is sequenced **daemon (life-ops) → chat surface → coding pack**, not “three products at once.” Positioning and replacement priority: [architecture/positioning.md](architecture/positioning.md).

**Recently hardened (2026-07-23):** module splits in hot-path crates; Tier-1 live conformance (L1–L6/L8/L10; L7/L9 open); MCP connection pooling (`tuning.mcp_pooling`, default on). Details in [roadmap/current.md](roadmap/current.md) and [research/archive/grok-architecture-analysis-2026-07-22.md](research/archive/grok-architecture-analysis-2026-07-22.md).

---

## Link conventions (GitHub)

- Prefer **relative** links from the linking file (e.g. `../architecture/overview.md`).
- Do not use site-root paths like `/docs/...` (they break on GitHub blob views).
- Archive pages must not be linked as “current” architecture without a status banner.

**Last updated:** 2026-07-23 — docs taxonomy + freshness pass after architecture-hardening commit.
