---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0007
open_items: false
---

# ADR-0007: Monorepo vs. Separate Repos Strategy

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0007 (`monorepo-workspace`)

## Context

"Loose coupling via separate repos" conflicts with heavy use of shared crates (`hook-common`, guards, types).

## Decision

**One Cargo workspace (monorepo)** for the Liberado system — `common`, `hook-common`, `main-agent` (daemon), `liberado-dispatcher`, `liberado-memory-mcp`, the MCP crates, the hook crates, and `tui`. Crate boundaries are kept clean so any crate can be extracted to its own repo later with low friction. **External dependencies** (`turbovault`, `turbomcp` and its crates) are *not* vendored into the workspace — they are consumed as **path dependencies during co-development** (the repos are checked out as siblings and Shiloh actively contributes to both) and **pinned to crates.io versions for release builds**. The existing `liberado-tool-helper-mcp` repo is folded in as the `liberado-memory-mcp` crate at implementation time. This resolves the original "loose coupling via separate repos vs. shared crates" tension in favor of shared crates now, extraction later if ever needed.

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

**Why it matters**: "Loose coupling via separate repos" conflicts with heavy use of shared crates (`hook-common`, guards, types).

**Recommended path**:
- Commit to a **Cargo workspace (monorepo)** for v1–v2.
- Design crate boundaries cleanly so extraction to separate repos later is low-friction if needed.
- Drop the "separate repos from day one" aspiration for now; it creates version skew and complexity without enough benefit yet.

**Status**: Complete

Decision 7: **One Cargo workspace (monorepo)** for the Liberado system — `common`, `hook-common`, `main-agent` (daemon), `liberado-dispatcher`, `liberado-memory-mcp`, the MCP crates, the hook crates, and `tui`. Crate boundaries are kept clean so any crate can be extracted to its own repo later with low friction. **External dependencies** (`turbovault`, `turbomcp` and its crates) are *not* vendored into the workspace — they are consumed as **path dependencies during co-development** (the repos are checked out as siblings and Shiloh actively contributes to both) and **pinned to crates.io versions for release builds**. The existing `liberado-tool-helper-mcp` repo is folded in as the `liberado-memory-mcp` crate at implementation time. This resolves the original "loose coupling via separate repos vs. shared crates" tension in favor of shared crates now, extraction later if ever needed.
