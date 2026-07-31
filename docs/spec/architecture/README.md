# Architecture

Living description of **how Liberado works today**. Historical plans belong in [roadmap/archive](../../future-work/archive/README.md).

| Doc | Role |
|-----|------|
| [overview.md](overview.md) | Cold-start system map: pillars, daemon loop, safety |
| [sessions.md](sessions.md) | Unified `Session` model (D7) — load-bearing abstraction |
| [contracts.md](contracts.md) | Narrow waists / frozen seams inventory |
| [modularity.md](modularity.md) | Seam plan and “could someone use just this crate?” |
| [failure-modes.md](failure-modes.md) | Five recurring bug classes (read before safety PRs) |
| [agentic-loops.md](agentic-loops.md) | Kernel vs domain packs; goals, verifiers, subagents |
| [positioning.md](positioning.md) | Why daemon → chat → coding sequencing |
| [channels-and-interactivity.md](channels-and-interactivity.md) | Surfaces, AskHuman, messaging seams |
| [session-surface-contract.md](session-surface-contract.md) | How surfaces should treat sessions |
| [verifiers.md](verifiers.md) | Completeness gates / CI-in-the-loop sketch |

**Crate-level docs:** [reference/crate-map.md](../../spec/reference/crate-map.md) and `crates/*/ARCHITECTURE.md`.

**Related:** [specs/](spec/README.md) for decision log and detailed specs; [roadmap/roadmap.md](../../roadmap.md) for open work.
