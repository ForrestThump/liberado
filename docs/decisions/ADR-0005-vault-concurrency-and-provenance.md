---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0005
open_items: false
---

# ADR-0005: Vault Concurrency, Write Provenance, and Loop-Breaking

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0005 (`vault-concurrency-and-provenance`)

## Context

The vault is the shared database with many writers (human in Obsidian, main agent, subagents, hooks). Without clear rules, we risk write races, data loss, and infinite reaction loops.

## Decision

Resolved in `liberado-vault-concurrency-spec.md`. Summary:
- **Provenance lives on the Turbovault audit log, not frontmatter** (frontmatter is last-writer-only state and goes stale on direct Obsidian edits). Rides on `AuditEntry.metadata._liberado_provenance` today; migrates to a typed field if the upstream proposal lands. `source` + `correlation_id` are mandatory on every agent write.
- **Loop-breaking via Approach A (consumer-side hash join)**: attribute an observed change by matching `sha256(nfc(content))` against the `after_hash` of the latest audit entry for that path. Match + non-human + recent ? suppress; no match ? external/human edit ? react. Robust to races, coalescing, and human-edits-after-agent. A bounded seen-correlation set + child correlation IDs break cross-hook A?B?A chains; `MAX_REACTION_DEPTH` halts cascades.
- **Consume Turbovault's native subscription (PR #24), not a custom emitter.** The daemon holds one subscription and does the hash-join + de-loop **centrally**, then routes already-attributed events to thin hooks. This supersedes the hand-built `vault-change-emitter` in `life-os-architecture.md` §5 (non-vault triggers still POST webhooks directly).
- **Conc…

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- `liberado-vault-concurrency-spec.md`
- `life-os-architecture.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: The vault is the shared database with many writers (human in Obsidian, main agent, subagents, hooks). Without clear rules, we risk write races, data loss, and infinite reaction loops.

**Current state in design**: "Hash-protected writes" mentioned. Vault-emitter is responsible for change detection but its full responsibilities are underspecified.

**Open questions**:
- Which paths are human-only vs. agent-writable?
- How do we handle concurrent edits (Obsidian + agent)?
- How do we prevent reaction loops (agent writes ? emitter fires hook ? hook writes ? emitter fires again)?
- What provenance tagging is required on agent-originated changes?

**Recommended path**:
- Make the **vault-emitter** a first-class, well-specified component (not "thin").
- Require **provenance tagging** on all agent writes (e.g., frontmatter field `agent_write: true` + correlation ID).
- Build **debouncing + loop detection** into the emitter from day one.
- Define clear human-vs-agent write boundaries per zone.
- Use hash-protected writes + optimistic concurrency where possible.
- Document this explicitly — it is load-bearing for reliable background behavior.

**Status**: Complete

Decision 5: Resolved in `liberado-vault-concurrency-spec.md`. Summary:
- **Provenance lives on the Turbovault audit log, not frontmatter** (frontmatter is last-writer-only state and goes stale on direct Obsidian edits). Rides on `AuditEntry.metadata._liberado_provenance` today; migrates to a typed field if the upstream proposal lands. `source` + `correlation_id` are mandatory on every agent write.
- **Loop-breaking via Approach A (consumer-side hash join)**: attribute an observed change by matching `sha256(nfc(content))` against the `after_hash` of the latest audit entry for that path. Match + non-human + recent ? suppress; no match ? external/human edit ? react. Robust to races, coalescing, and human-edits-after-agent. A bounded seen-correlation set + child correlation IDs break cross-hook A?B?A chains; `MAX_REACTION_DEPTH` halts cascades.
- **Consume Turbovault's native subscription (PR #24), not a custom emitter.** The daemon holds one subscription and does the hash-join + de-loop **centrally**, then routes already-attributed events to thin hooks. This supersedes the hand-built `vault-change-emitter` in `life-os-architecture.md` §5 (non-vault triggers still POST webhooks directly).
- **Concurrency stays optimistic** with the structured `ConcurrentModification { path, expected, actual }` error; agents re-read and retry (bounded) rather than overwrite.
- **Per-zone write classes** (`human_only` / `agent_writable` / `proposal_only` / `shared`) enforced at the MCP/hook boundary; unlisted zones default to `proposal_only` (fail safe).
- **Idempotency**: correlation ID is the idempotency key; vault-as-journal (pending?working?done markers) makes redelivery safe.
- **Attribution is best-effort, never a security boundary** (`None` = treat as external). Security stays with the Decision 4 capability/zone model.
- No upstream merge blocks the architecture — every upstream dependency has a working fallback behind a thin adapter (see spec §8.1).

---

## Tier 2: High-Impact Seams (Decide Before Building Relevant Components)
