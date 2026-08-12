---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0015
open_items: false
---

# ADR-0015: Frontmatter Schema Validation + Migration

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0015 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

Open, per-zone frontmatter schemas enforced on agent writes and normalized lazily for humans. Universal baseline on agent writes includes type and created; provenance stays out of frontmatter (audit log owns it). Schemas declared in config; migration is lazy with optional batch tooling.

## Consequences

Humans keep zero-friction capture in inbox. Agents get reject-and-retry on schema violations. Extra keys remain allowed for Dataview/Bases.

## Rejected alternatives

Closed schemas that reject unknown keys. Blocking human writes on missing frontmatter. Storing provenance history in frontmatter.

## Implementation and tests

- See crate Rustdoc and tests for the current implementation of this decision.

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

Decide validation approach and migration strategy for frontmatter fields before the vault grows large.

**Status**: Complete

Decision 15: **Open, per-zone schemas, tiered by writer** — designed so schema never fights zero-friction capture.

- **Open schemas**: validation checks that *required* keys are present and well-typed; extra keys are always allowed (ad-hoc Dataview/Bases fields stay free). Structural, not closed.
- **Enforce on agent writes; normalize human writes, never block them.** Agent-created/processed notes must satisfy the zone schema (cheap for the agent, and satisfied by construction when it uses Turbovault `create_from_template` — **templates are the schema's concrete form**). Human-written notes — especially `inbox/` capture — have **no required frontmatter**; the system **backfills** schema keys when it next processes/files the note. Humans never hit friction.
- **Universal baseline** (on agent writes): `type` (task | decision | goal | review | proposal | knowledge | …) and `created`. `type` is the highest-value key — it drives Dataview/Bases queries, ContextPolicy lookups (`type=goal AND status=active`), and dispatcher routing. **Per-zone adds**: `status` for anything with a lifecycle; `proposals/` uses the full schema from Decision 11; `goals/`/`decisions/` carry the ISA-style success-criteria/outcome fields.
- **Explicitly NOT in frontmatter**: provenance / edit history — Turbovault's audit log already owns per-write events. Frontmatter holds current *state* only (same reasoning as the concurrency spec).
- **Validation** happens at the MCP write boundary (alongside capability checks): agent write violating schema ? reject-and-retry; human write ? accept + normalize lazily.
- **Migration**: optional `schema_version` makes migrations idempotent. **Lazy by default** (normalize on next write/process), with an **on-demand/maintenance batch migration** using Turbovault `inspect_frontmatter` / `query_frontmatter_sql` to find stale notes and `batch_execute` to update; the git backstop (maintenance spec) makes big-bang migration safe if ever wanted.
- Schemas are **declared in config** (`policy`/schema section — Decision 14), one authoritative definition.
