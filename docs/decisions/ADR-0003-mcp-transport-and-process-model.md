---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0003
open_items: false
---

# ADR-0003: MCP Transport and Process Model (Multiple Consumers)

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0003 (`mcp-transport-and-process-model`)

## Context

The main agent (via liberado), subagents, and hooks may all need to invoke the same MCPs (e.g., tasks-mcp). Stdio is simple but couples lifecycle and makes sharing difficult.

## Decision

MCP Transport and Process Model (Finalized)
Decision:
Support both HTTP/SSE and stdio transports, with a strong preference for long-running HTTP/SSE MCP services. Stateless MCPs are preferred. Stateful MCPs are allowed when necessary, but must use narrow resource-level locking rather than broad MCP-level locks.
Rationale
Multiple consumers (main agent via liberado, subagents, and hooks) will eventually need to interact with MCPs concurrently. A pure stdio model creates lifecycle and sharing problems in this scenario. Long-running HTTP/SSE services make concurrent access more natural while still allowing capability narrowing.
Stateless (or narrowly stateful) MCPs are dramatically easier to reason about, test, and scale. However, some capabilities genuinely require state (e.g., sessionful connections or complex in-memory coordination), so we should not ban stateful MCPs outright.
When state is required, broad locks on the entire MCP would severely limit concurrency. Narrow locking at the resource or zone level is a better fit with the capability-based model developed in Decision 4.

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- See crate Rustdoc and tests for the current implementation of this decision.

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: The main agent (via liberado), subagents, and hooks may all need to invoke the same MCPs (e.g., tasks-mcp). Stdio is simple but couples lifecycle and makes sharing difficult.

**Current state in design**: "stdio or SSE" left open. Multiple consumers not yet addressed.

**Open questions**:
- Do MCPs run as long-lived services (HTTP/SSE) or are they spawned per-caller (stdio)?
- Are MCPs shared singletons or per-caller instances?
- How does this interact with capability filtering per caller (main agent vs subagent vs hook)?

**Recommended path**:
- Prefer **long-running HTTP/SSE MCP services** for v1 and beyond (easier sharing, connection model, and capability enforcement at the boundary).
- Use stdio only for very simple/one-off MCPs if needed.
- Design MCPs to be **stateless or narrowly stateful** so multiple callers can use them safely.
- Capability filtering happens at dispatch time (liberado or hook) before calling the MCP.

**Status**: Complete

Decision 3: MCP Transport and Process Model (Finalized)
Decision:
Support both HTTP/SSE and stdio transports, with a strong preference for long-running HTTP/SSE MCP services. Stateless MCPs are preferred. Stateful MCPs are allowed when necessary, but must use narrow resource-level locking rather than broad MCP-level locks.
Rationale
Multiple consumers (main agent via liberado, subagents, and hooks) will eventually need to interact with MCPs concurrently. A pure stdio model creates lifecycle and sharing problems in this scenario. Long-running HTTP/SSE services make concurrent access more natural while still allowing capability narrowing.
Stateless (or narrowly stateful) MCPs are dramatically easier to reason about, test, and scale. However, some capabilities genuinely require state (e.g., sessionful connections or complex in-memory coordination), so we should not ban stateful MCPs outright.
When state is required, broad locks on the entire MCP would severely limit concurrency. Narrow locking at the resource or zone level is a better fit with the capability-based model developed in Decision 4.
Key Points

Primary transport: Long-running HTTP/SSE (or WebSocket) MCP services. This is the recommended model for most MCPs.
Secondary transport: stdio — fully supported, particularly useful for simple or early-stage MCPs.
Stateless preferred: Most MCPs should be designed to be stateless or use optimistic concurrency where possible.
Stateful MCPs: Allowed, but should use narrow resource-level locking (e.g., per Zone, per resource ID, or per specific object) instead of locking the entire MCP.
Concurrency support: The architecture should enable multiple subagents (and the main agent) to call MCPs concurrently when their capabilities and locked resources do not conflict.
Documentation requirement: Every MCP should clearly declare whether it is stateless or stateful and describe its concurrency/locking behavior.

Interaction with Other Decisions

Daemon-first (Decision 2): Long-running HTTP MCP services integrate naturally with the daemon model.
Capability narrowing (Decision 4): Narrow resource locking pairs well with dynamic capability narrowing during dispatch. Liberado can grant reduced capabilities and reduced locking scope to subagents.
Future multi-agent work: This model supports the goal of allowing multiple agents/subagents to operate concurrently without excessive contention.

Trade-offs
Advantages:

Good support for concurrent subagent execution.
Cleaner sharing of MCPs across different callers.
More scalable and future-proof than a pure stdio model.
Narrow locking preserves concurrency better than coarse-grained locks.

Disadvantages:

HTTP/SSE services are slightly more complex to implement than simple stdio MCPs.
Stateful MCPs still require careful design around locking and recovery.

Final Recommendation
Adopt HTTP/SSE as the primary transport with support for stdio. Prefer stateless MCPs, but allow stateful ones when genuinely needed — using narrow resource-level locking rather than broad MCP locks. This provides the best balance between simplicity, concurrency, and future multi-agent support.
