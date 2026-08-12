---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0006
open_items: false
---

# ADR-0006: Event Delivery Semantics, Idempotency, and Durability

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0006 (`event-delivery-and-idempotency`)

## Context

Bare HTTP POST is at-most-once. Background autonomy that silently drops work on restart is not acceptable.

## Decision

Both delivery paths (Turbovault subscription with drop-and-resync; webhook POST) are **at-most-once**, so reaction handlers are **idempotent by construction**. The `correlation_id` carried on every standardized event is the **idempotency key**. Before acting, a hook checks a durable journal marker (`.liberado/reactions/<correlation_id>.json`: pending ? working ? done) — redelivery re-enters at the existing marker instead of double-acting. **Vault-as-journal** is the v1 durability story; no separate durable queue. On subscription **drop/overflow**, the contract is *resync from authoritative state* (bounded re-scan of the hook's zone), never "assume we saw every event." **Event ordering is not guaranteed** — handlers must converge regardless of order (idempotent, not order-dependent). A durable queue is deferred until vault-centric journaling proves insufficient.

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- `liberado-vault-concurrency-spec.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: Bare HTTP POST is at-most-once. Background autonomy that silently drops work on restart is not acceptable.

**Recommended path**:
- Design hook reaction handlers to be **idempotent** from the start (use correlation IDs + check-if-already-processed).
- Use the **vault itself as the durable journal** where possible (write intended work as a pending item, then process).
- For higher reliability later, consider a small durable queue, but keep v1 vault-centric.

**Status**: Complete (specified in `liberado-vault-concurrency-spec.md` §7).

Decision 6: Both delivery paths (Turbovault subscription with drop-and-resync; webhook POST) are **at-most-once**, so reaction handlers are **idempotent by construction**. The `correlation_id` carried on every standardized event is the **idempotency key**. Before acting, a hook checks a durable journal marker (`.liberado/reactions/<correlation_id>.json`: pending ? working ? done) — redelivery re-enters at the existing marker instead of double-acting. **Vault-as-journal** is the v1 durability story; no separate durable queue. On subscription **drop/overflow**, the contract is *resync from authoritative state* (bounded re-scan of the hook's zone), never "assume we saw every event." **Event ordering is not guaranteed** — handlers must converge regardless of order (idempotent, not order-dependent). A durable queue is deferred until vault-centric journaling proves insufficient.
