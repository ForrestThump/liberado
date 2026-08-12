---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0009
open_items: false
---

# ADR-0009: How Hook Messages Reach the Main Agent

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0009 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

Hook and detached-subagent results reach the main agent only via vault-mediated artifacts (provenance + correlation_id) into agent-writable surfacing zones. ContextPolicy Job B surfaces unseen items; hooks never push into the daemon or know the main loop.

## Consequences

Maximum decoupling: a hook's only outbound contract is writing a vault artifact. Direct high-priority push channels stay deferred until a proven need.

## Rejected alternatives

Direct push from hooks into the daemon event loop as the v1 primary path.

## Implementation and tests

- `liberado-context-policy-spec.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: Affects coupling between hooks and the core loop.

**Recommended path**:
- Primary path: **vault-mediated** (hooks write structured artifacts/summaries; ContextPolicy surfaces relevant items). Maximum loose coupling.
- Allow optional direct channel for high-priority cases later.

**Status**: Complete (surfacing mechanism in `liberado-context-policy-spec.md` §2 Job B).

Decision 9: **Vault-mediated only** for v1. Hooks and detached subagents **write structured artifacts** (with provenance + `correlation_id`) to agent-writable surfacing zones (`reviews/`, `proposals/`, hook output locations); they do **not** push into the daemon or know anything about the main loop. ContextPolicy's **per-turn Job B** surfaces unseen items (queried by a since-last-seen cursor / `surfaced: false` frontmatter, marked surfaced after showing). This keeps hooks maximally decoupled — a hook's only outbound contract is "write a vault artifact." A direct high-priority push channel is **deferred** until a real need (e.g. an urgent interrupt that can't wait for the next turn) is proven.
