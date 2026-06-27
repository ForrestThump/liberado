# Liberado Wiki

Welcome to the Liberado documentation wiki — single source of truth for humans and agents.

## Quick Start
New users should begin here:

- [Getting Started → Quickstart](docs/getting-started/quickstart.md)

## High-Level
- [Overview & Architecture](docs/architecture/overview.md) — system pillars, loop, and safety model
- [Positioning](docs/architecture/positioning.md) — the thesis and how Liberado is grounded against the free alternatives
- [Roadmap (current)](docs/roadmap/current.md) — what is committed and in-flight
- [Interface / API Reference](docs/reference/api.md) — only public contract (HTTP + SSE)

## Deep Dive
- [Architecture Components](docs/architecture/) — per-crate `ARCHITECTURE.md` pages
- [Modularity — the seam plan](docs/architecture/modularity.md) — the per-crate "could someone use just this?" test and the mesh seams
- [Specifications & Decisions](docs/specs/) — detailed design specs and architecture decisions
- [Ideas & Experiments](docs/ideas/) — competitive analysis (Hermes, mesh), concurrency thoughts, future directions
- [Contributing](docs/contributing/agents.md) — build/run instructions and agent guide

## Philosophy
Everything here is meant to be **navigable by agents and humans alike**. Each page links to its neighbors; no orphan files at the root.

---

**Last updated**: 2026-06-26 — added Positioning and Modularity pages; matured pillars (vault = default plugin) and the phased roadmap.