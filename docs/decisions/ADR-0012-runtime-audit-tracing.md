---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0012
open_items: false
---

# ADR-0012: Runtime Audit / Tracing Substrate

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0012 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

Use tracing with structured spans across daemon, dispatcher, MCPs, and hooks. Keep two trails distinct: Turbovault audit log (write provenance) vs Liberado runtime trace (dispatch, tools, hooks, errors) as append-only JSONL outside vault markdown.

## Consequences

Dispatch quality and quadratic-savings theses become measurable. High-volume traces do not pollute the vault change stream the system reacts to.

## Rejected alternatives

Writing runtime traces into vault notes. Conflating write provenance with behavioral audit.

## Implementation and tests

- `liberado-dispatch-logic-spec.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: "Fully auditable" currently only covers code + git state. Runtime behavior (dispatch decisions, tool calls, hook reactions) is invisible.

**Recommended path**:
- Use `tracing` with structured spans from the beginning.
- Consider a durable append-only sink (even a simple file or vault-based log) for early usage data.
- Instrument liberado dispatch decisions especially — this data tells us whether the quadratic savings and dispatch logic are working.

**Status**: Complete

Decision 12: **`tracing` with structured spans** across the daemon, dispatcher, MCPs, and hooks from day one. **Dispatch decisions are instrumented specifically** (goal hash, retrieved guidance ids, action, confidence, rationale, guard downgrades, await/detach, outcome — `liberado-dispatch-logic-spec.md` §9) — this is the data that validates the routing and quadratic-savings theses.

**Two distinct trails, deliberately not conflated**:
1. **Turbovault audit log** (`turbovault-audit`, already exists): vault **write provenance** — before/after hashes + provenance metadata. Powers loop-breaking (Decision 5). A property of *vault writes*.
2. **Liberado runtime trace** (new): dispatch decisions, tool calls, hook reactions, errors. A property of *system behavior*.

**Sink**: the runtime trace is **append-only JSONL outside the vault markdown** (a daemon trace dir / gitignored `.liberado/trace/`), **never** into vault notes — high-volume trace writes would pollute the very change stream the system reacts to (the same lesson that put provenance on the audit log, not frontmatter). A richer sink (e.g. structured DB) is a later, non-blocking upgrade.

---

## Tier 3: Important but More Contained Decisions
