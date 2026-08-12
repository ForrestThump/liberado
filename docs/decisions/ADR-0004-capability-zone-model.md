---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0004
open_items: false
---

# ADR-0004: Capability / Zone Model — Concrete Data Structures and Semantics

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0004 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

Capability and zone model is first-class shared infrastructure (common crate types): Zone (path prefix/glob), Capability, CapabilitySet/Policy, and a check function at every MCP/hook boundary. Permissions are defined simply (YAML-style grants); zones cover vault areas. Capability may be narrowed at dispatch but never expanded. Enforcement is at each MCP/hook entry, not ambient authority.

## Consequences

Security retrofits are avoided by defining types before MCP/hook code proliferates. Every boundary must call the guard. Static per-component grants plus dispatch-time narrowing are the initial authority path.

## Rejected alternatives

Leaving path/zone guards as prose without concrete types. Expanding capabilities after dispatch. Relying on ambient process authority instead of per-call checks.

## Implementation and tests

- `liberado-permissions-idea.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
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
