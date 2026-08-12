---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0008
open_items: false
---

# ADR-0008: Subagent Execution Model (Isolation Level)

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0008 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

In-process subagents (tokio tasks) for v1, capped by MAX_CONCURRENT_SUBAGENTS, spawned through a Subagent boundary the dispatcher does not specialize by isolation mode. Containment is capability narrowing at the MCP boundary plus secret isolation—not OS sandboxing. Isolation remains configurable for a later out-of-process upgrade.

## Consequences

Adequate for trusted in-house prompts; not adversarial isolation. Context slices stay disjoint for KV-cache control. Process isolation is a config/path change, not a dispatch rewrite.

## Rejected alternatives

Out-of-process sandbox for every subagent in v1. Letting the dispatcher branch on in-process vs out-of-process. Ambient authority inside the daemon for subagent tools.

## Implementation and tests

- `liberado-dispatch-logic-spec.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: Affects security isolation, complexity, resource usage, and KV-cache pressure on local inference.

**Recommended path**:
- Start with **in-process subagents with strong capability filtering** for v1 simplicity.
- Design the interface so heavier subagents can later move to separate processes without changing dispatch logic.
- Optimize context slices for disjointness to control KV-cache memory and realize quadratic savings.

**Status**: Complete (interfaces in `liberado-dispatch-logic-spec.md` §4, §10).

Decision 8: **In-process subagents** (tokio tasks in the daemon) for v1, capped at `MAX_CONCURRENT_SUBAGENTS` (default 2) for KV-cache/homelab bounds. They are spawned through a `Subagent` boundary that takes `(goal, CapabilitySet, allowed_mcps, success_criteria, model, correlation_id)` and returns a `Report` — **the dispatcher never knows whether a subagent ran in-process or out-of-process**, so moving heavy/experimental subagents to separate processes later requires no dispatch-logic change. **Isolation model, stated honestly**: in-process subagents share the daemon's memory space, so their *only* containment is **capability narrowing enforced at the MCP boundary** (no ambient authority — a subagent holds only a narrowed MCP client) plus secret isolation (raw secrets never reach any subagent; inference via the daemon). This is "trust-the-hand-audited-code" isolation, adequate for v1 because all subagent code and prompts are ours; it is **not** adversarial isolation. Out-of-process subagents (OS sandbox) are the upgrade path if/when subagents ever run less-trusted prompts. Context slices are kept disjoint (goal + narrowed schemas + work context only) for KV-cache control and the quadratic-prefill savings. **Isolation level is configurable** (`subagent.isolation = in_process | out_of_process`, default `in_process`) in the single-source config (Decision 14), so scaling to process isolation is a config change, not a source edit.
