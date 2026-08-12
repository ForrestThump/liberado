---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0012
open_items: false
---

# ADR-0012: Runtime Audit / Tracing Substrate

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0012 (`runtime-audit-tracing`)

## Context

"Fully auditable" currently only covers code + git state. Runtime behavior (dispatch decisions, tool calls, hook reactions) is invisible.

## Decision

**`tracing` with structured spans** across the daemon, dispatcher, MCPs, and hooks from day one. **Dispatch decisions are instrumented specifically** (goal hash, retrieved guidance ids, action, confidence, rationale, guard downgrades, await/detach, outcome — `liberado-dispatch-logic-spec.md` §9) — this is the data that validates the routing and quadratic-savings theses.

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- `liberado-dispatch-logic-spec.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
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
