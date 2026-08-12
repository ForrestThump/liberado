---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0004
open_items: false
---

# ADR-0004: Capability / Zone Model — Concrete Data Structures and Semantics

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0004 (`capability-zone-model`)

## Context

This is the foundation of the entire security and containment story. "Path/zone guards" and "capability gates" are mentioned throughout but never defined. Retrofits here are extremely expensive.

## Decision

See body for full decision text.

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- `liberado-permissions-idea.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: This is the foundation of the entire security and containment story. "Path/zone guards" and "capability gates" are mentioned throughout but never defined. Retrofits here are extremely expensive.

**Current state in design**: Mentioned repeatedly but underspecified. No concrete types yet.

**Open questions**:
- What exactly is a "Zone"? (Path glob set? Named region of the vault? Hierarchical?)
- What is the capability grammar? (e.g., `Read(tasks/*)`, `Write(decisions/)`, `Invoke(tasks-mcp:complete)`)
- Who is the authority that grants capabilities? (Static per-component config? Dynamic from liberado at dispatch time? Vault-based policy?)
- How are capabilities passed and checked at every boundary (MCP, hook, subagent spawn)?

**Recommended path**:
- Define concrete types in a shared `common` crate **before writing any MCP or hook code**:
  - `Zone` (simple path prefix + optional glob for v1)
  - `Capability` (enum or structured type)
  - `CapabilitySet` / `Policy`
  - `check_capability(subject, action, target)` function
- Start with **static per-component + dispatch-time grants** from liberado.
- Make every MCP and hook call the guard on entry.
- This single artifact unblocks a huge amount of the security model.

**Status**: Complete

Defined as `liberado-permissions-idea.md`. The enforcement boundary is at each MCP / hook. Permission can be narrowed at dispatch, but never expanded. Simple yaml defition of permissions. Zones for areas of permission.
