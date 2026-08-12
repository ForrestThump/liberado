---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0007
open_items: false
---

# ADR-0007: Monorepo vs. Separate Repos Strategy

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0007 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

One Cargo workspace (monorepo) for Liberado crates with clean boundaries for later extraction. External deps (turbovault, turbomcp) stay out of the workspace as path deps during co-development and crates.io pins for release. Fold prior tool-helper MCP work into the workspace as named crates at implementation time.

## Consequences

Shared types and versioning stay coherent. Separate-repos-from-day-one is deferred to avoid version skew without enough benefit.

## Rejected alternatives

Separate repos for every crate from day one. Vendoring turbovault/turbomcp into this workspace.

## Implementation and tests

- See crate Rustdoc and tests for the current implementation of this decision.

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: "Loose coupling via separate repos" conflicts with heavy use of shared crates (`hook-common`, guards, types).

**Recommended path**:
- Commit to a **Cargo workspace (monorepo)** for v1–v2.
- Design crate boundaries cleanly so extraction to separate repos later is low-friction if needed.
- Drop the "separate repos from day one" aspiration for now; it creates version skew and complexity without enough benefit yet.

**Status**: Complete

Decision 7: **One Cargo workspace (monorepo)** for the Liberado system — `common`, `hook-common`, `main-agent` (daemon), `liberado-dispatcher`, `liberado-memory-mcp`, the MCP crates, the hook crates, and `tui`. Crate boundaries are kept clean so any crate can be extracted to its own repo later with low friction. **External dependencies** (`turbovault`, `turbomcp` and its crates) are *not* vendored into the workspace — they are consumed as **path dependencies during co-development** (the repos are checked out as siblings and Shiloh actively contributes to both) and **pinned to crates.io versions for release builds**. The existing `liberado-tool-helper-mcp` repo is folded in as the `liberado-memory-mcp` crate at implementation time. This resolves the original "loose coupling via separate repos vs. shared crates" tension in favor of shared crates now, extraction later if ever needed.
