---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0001
open_items: false
---

# ADR-0001: Liberado Invocation Model + Inference Responsibility

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0001 (`invocation-model-and-inference`)

## Context

This is the central architectural question. It determines whether liberado is a simple tool, an out-of-band orchestrator, or a full agent with its own inference. It directly affects token accounting, latency, framework fit (Rig vs custom loop), and how we realize quadratic prefill savings.

## Decision

Liberado operates as an out-of-band intelligent dispatcher agent. It has access to the full MCP catalog (names + short descriptions) and receives minimal, goal-specific context from the main agent. It can:

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- `liberado-dispatch-logic-spec.md`
- `life-os-architecture.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: This is the central architectural question. It determines whether liberado is a simple tool, an out-of-band orchestrator, or a full agent with its own inference. It directly affects token accounting, latency, framework fit (Rig vs custom loop), and how we realize quadratic prefill savings.

**Current state in design**: Treated as an "intelligent goal-understanding dispatcher" that can do direct MCP invocation or subagent dispatch. Not fully specified whether it runs its own LLM call.

**Open questions**:
- Is liberado invoked as a normal tool call inside the main agent's tool-calling loop, or does the main loop intercept intent out-of-band and hand it to liberado?
- Does liberado perform its own inference (LLM call) to classify simple vs. complex goals and choose strategy?
- How does liberado receive the necessary context without duplicating large parts of the main agent's context?

**Recommended path**:
- Make liberado a **separate, narrowly-scoped agent** (with its own lightweight context policy) that the main loop calls out-of-band when needed.
- It performs a small, focused inference step (goal classification + dispatch strategy) using a fast/cheaper model when possible.
- Optimize for **disjoint context partitions** to realize real quadratic savings (dispatcher sees goal + filtered tool catalog; subagent sees goal + chosen schemas + work context; minimal overlap).
- This is more powerful than a pure tool call and justifies the extra hop for local inference and long-context regimes.

**Status**: Complete.

Decision 1:
Liberado operates as an out-of-band intelligent dispatcher agent. It has access to the full MCP catalog (names + short descriptions) and receives minimal, goal-specific context from the main agent. It can:

Directly execute simple, high-confidence tool calls,
Spawn narrowly-scoped subagents with disjoint context, or
Escalate back to the main agent with structured uncertainty signals when clarification or higher-level judgment is needed.

The main agent context remains protected from tool definitions, internal dispatch reasoning, and low-level tool execution traces.

**Routing detail resolved in `liberado-dispatch-logic-spec.md`**: the dispatcher chooses among four terminal actions — `ExecuteDirect`, `DispatchSubagent`, `Clarify` (to the main agent), and `Report` (the return type of the first two). Choice is made by a 5-step pipeline (retrieve procedural guidance ? classify via small inference ? downgrade-only deterministic guards ? act ? record outcome). Correctness is engineered, not assumed: routing is **safe-by-default** (uncertainty degrades toward Clarify/proposal, never toward an irreversible action), guards can only *downgrade* risk (capability/zone-write-class/consequence/reaction-depth/confidence), and the decision is a typed, traced, eval-tested artifact (Decisions 12, 16). The component split (new `liberado-dispatcher` consuming the renamed `liberado-memory-mcp` for general + procedural memory) is recorded in `life-os-architecture.md` §2.
