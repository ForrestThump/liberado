# Liberado Wiki

Welcome to the Liberado documentation wiki — single source of truth for humans and agents.

## Quick Start
New users should begin here:

- [Getting Started → Quickstart](docs/getting-started/quickstart.md)

## High-Level
- [Overview & Architecture](docs/architecture/overview.md) — system pillars, loop, safety model, and the kernel · domain packs · stores · surfaces vocabulary
- [Sessions](docs/architecture/sessions.md) — **everything is a `Session`** (D7): one type, one store, one list. Chats, goal sessions, cron runs and subagents differ by *attributes*, not subtypes — `goal: Option` is the whole difference. Start here; it is the load-bearing abstraction.
- [Contracts](docs/architecture/contracts.md) — the narrow-waist inventory: the frozen seams that *are* the architecture
- [Crate map](docs/reference/crate-map.md) — generated per-crate layer/deps/description table (`scripts/gen-crate-map.ps1`)
- [Agentic Loops](docs/architecture/agentic-loops.md) — kernel vs domain packs; goal sessions, verifiers, subagents
- [Verifiers & completeness gates](docs/architecture/verifiers.md) — CI-in-the-loop schema/trait sketch (domain-agnostic)
- [Positioning](docs/architecture/positioning.md) — the thesis and how Liberado is grounded against the free alternatives
- [Roadmap (current)](docs/roadmap/current.md) — what is committed and in-flight
- [Session focus plan](docs/roadmap/archive/session-focus-plan.md) — *how* the Session model was built, slice by slice (S1–S7, store convergence, forking). History; the model itself is in [Sessions](docs/architecture/sessions.md).
- [Loops plan](docs/roadmap/loops-plan.md) — scheduled recurrence over goals (loop = scheduler + series memory, not a fourth engine); vocabulary: turn loop ⊂ goal ⊂ loop ⊂ meta-loop
- [Agentic orchestration plan](docs/roadmap/rust-native-agentic-coder-plan.md) — implementation roadmap (coding pack first)
- [Agentic mesh hygiene audit](docs/roadmap/archive/agentic-mesh-hygiene-audit-2026-07-10.md) — coupling, duplication, generality
- [Architecture alignment audit](docs/roadmap/archive/architecture-alignment-audit-2026-07-11.md) — dependency-graph verification, the "mesh" framing verdict, complexity-management plan
- [Interface / API Reference](docs/reference/api.md) — only public contract (HTTP + SSE)
- [Delegate dogfood notes](docs/roadmap/archive/delegate_dogfood_issues.md) — face/delegation capability, journals, hot-swap lessons

## Deep Dive
- [Architecture Components](docs/architecture/) — per-crate `ARCHITECTURE.md` pages
- [Modularity — the seam plan](docs/architecture/modularity.md) — the per-crate "could someone use just this?" test and the seam moves
- [Specifications & Decisions](docs/specs/) — detailed design specs and architecture decisions
- [Ideas & Experiments](docs/ideas/) — competitive analysis (Hermes, mesh), concurrency thoughts, future directions
- [Contributing](docs/contributing/agents.md) — build/run instructions and agent guide

## Philosophy
Everything here is meant to be **navigable by agents and humans alike**. Each page links to its neighbors; no orphan files at the root.

---

**Last updated**: 2026-07-13 — the unified Session model (D7): one converged store, session profiles + `AskHuman`, intake-first coding sessions, cron/hook/subagent runs recorded as background sessions, and conversation forking.