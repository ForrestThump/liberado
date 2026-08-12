---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0001
open_items: false
---

# ADR-0001: Liberado Invocation Model + Inference Responsibility

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0001 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

Liberado is an out-of-band intelligent dispatcher agent, not an in-loop tool call.

It holds the full MCP catalog (names + short descriptions) and receives minimal, goal-specific context from the main agent. It may:

- execute simple, high-confidence tool calls directly;
- spawn narrowly-scoped subagents with disjoint context; or
- escalate back to the main agent with structured uncertainty (Clarify) when needed.

The main agent context stays free of tool definitions, internal dispatch reasoning, and low-level execution traces. Routing is safe-by-default (uncertainty degrades toward Clarify/proposal, never toward irreversible action); guards may only downgrade risk.

## Consequences

Dispatch requires its own lightweight inference path and typed decision artifact. Token savings depend on keeping dispatcher and subagent contexts disjoint. Main-agent UX remains protected from tool catalog bloat, at the cost of an extra hop for classification and dispatch.

## Rejected alternatives

Pure in-loop tool-call invocation of liberado (no separate inference). Passing large overlapping main-agent context into the dispatcher (defeats quadratic prefill savings). Always escalating complex work without a direct-execute path.

## Implementation and tests

- `liberado-dispatch-logic-spec.md`
- `life-os-architecture.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
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
