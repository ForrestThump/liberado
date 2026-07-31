# Plans (archived index)

> **2026-07-31: plans were flattened.** The former `plans/` subfolder held a single plan plus this
> index; its one plan moved up to the `future-work/` root alongside every other active plan
> ([`../context-compaction-viewport-rearchitecture.md`](../context-compaction-viewport-rearchitecture.md)),
> and the folder was removed. This file is kept as the archived index of that folder.

The `plans/` folder was for design and re-architecture plans that are **not yet the shipped
contract**. Contrast with:

| Location | Role |
|----------|------|
| [roadmap.md](../../roadmap.md) | Product scoreboard + active/shipped roadmap plans |
| [architecture/](../../spec/architecture/README.md) | How the system works **now** |
| [specs/](../../spec/README.md) | Frozen decisions |
| Active plans in [`../`](../README.md) | Forward design for a known next evolution — not current truth until landed |

## Plan that used to live here

| Plan | Status | Domain |
|------|--------|--------|
| [context-compaction-viewport-rearchitecture.md](../context-compaction-viewport-rearchitecture.md) | Proposed (CH3.1) | Chat context compaction: side-summary + continue_from viewport |

When a plan ships, move a short "as built" note into the matching roadmap/architecture doc and
either archive the plan file or mark **Status: landed**.
