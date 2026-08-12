---
kind: index
status: active
authority: advisory
generated: true
domain: architecture
---

# Architecture Decision Records

Individual ADRs replace the former monolithic `docs/spec/architecture-decisions.md`.
ADRs are **mostly immutable**. A later change adds a new ADR that supersedes an old one.

Authority: accepted ADRs answer *why this design was selected*. Current behavior lives in
code, tests, and Rustdoc. Cross-crate contracts live in
[`docs/spec/architecture/contracts.md`](../spec/architecture/contracts.md).

| ADR | Title | Status | File |
|-----|-------|--------|------|
| ADR-0001 | Liberado Invocation Model + Inference Responsibility | accepted | [ADR-0001-invocation-model-and-inference.md](ADR-0001-invocation-model-and-inference.md) |
| ADR-0002 | Daemon-First vs. TUI-First Process Model | accepted | [ADR-0002-daemon-first-process-model.md](ADR-0002-daemon-first-process-model.md) |
| ADR-0003 | MCP Transport and Process Model (Multiple Consumers) | accepted | [ADR-0003-mcp-transport-and-process-model.md](ADR-0003-mcp-transport-and-process-model.md) |
| ADR-0004 | Capability / Zone Model — Concrete Data Structures and Semantics | accepted | [ADR-0004-capability-zone-model.md](ADR-0004-capability-zone-model.md) |
| ADR-0005 | Vault Concurrency, Write Provenance, and Loop-Breaking | accepted | [ADR-0005-vault-concurrency-and-provenance.md](ADR-0005-vault-concurrency-and-provenance.md) |
| ADR-0006 | Event Delivery Semantics, Idempotency, and Durability | accepted | [ADR-0006-event-delivery-and-idempotency.md](ADR-0006-event-delivery-and-idempotency.md) |
| ADR-0007 | Monorepo vs. Separate Repos Strategy | accepted | [ADR-0007-monorepo-workspace.md](ADR-0007-monorepo-workspace.md) |
| ADR-0008 | Subagent Execution Model (Isolation Level) | accepted | [ADR-0008-subagent-execution-model.md](ADR-0008-subagent-execution-model.md) |
| ADR-0009 | How Hook Messages Reach the Main Agent | accepted | [ADR-0009-hook-messages-via-vault.md](ADR-0009-hook-messages-via-vault.md) |
| ADR-0010 | Secrets Backend and Inter-Component Auth | accepted | [ADR-0010-secrets-and-inter-component-auth.md](ADR-0010-secrets-and-inter-component-auth.md) |
| ADR-0011 | Human-in-the-Loop / Proposal & Approval Boundary | accepted | [ADR-0011-proposal-and-approval-boundary.md](ADR-0011-proposal-and-approval-boundary.md) |
| ADR-0012 | Runtime Audit / Tracing Substrate | accepted | [ADR-0012-runtime-audit-tracing.md](ADR-0012-runtime-audit-tracing.md) |
| ADR-0013 | Provider Capability Floor / Minimum Contract | accepted | [ADR-0013-provider-capability-floor.md](ADR-0013-provider-capability-floor.md) |
| ADR-0014 | Single Source of Truth for Config / Topology | accepted | [ADR-0014-single-source-config.md](ADR-0014-single-source-config.md) |
| ADR-0015 | Frontmatter Schema Validation + Migration | accepted | [ADR-0015-frontmatter-schema-validation.md](ADR-0015-frontmatter-schema-validation.md) |
| ADR-0016 | Testing Seams for Nondeterministic Dispatch | accepted | [ADR-0016-testing-seams-for-dispatch.md](ADR-0016-testing-seams-for-dispatch.md) |
| ADR-0017 | Conversation History Store | accepted | [ADR-0017-conversation-history-store.md](ADR-0017-conversation-history-store.md) |
| ADR-0018 | Incremental Event-Bus Mesh (with checkpoints) | accepted | [ADR-0018-incremental-event-bus-mesh.md](ADR-0018-incremental-event-bus-mesh.md) |
| ADR-0019 | TurboVault as Privileged Plugin, not Hard Dependency | accepted | [ADR-0019-turbovault-as-privileged-plugin.md](ADR-0019-turbovault-as-privileged-plugin.md) |

## Writing a new ADR

1. Allocate the next number.
2. Create `ADR-NNNN-short-slug.md` with sections: status, date, context, decision,
   consequences, rejected alternatives, implementation/test links, supersedes/superseded-by.
3. If replacing an older decision, set **superseded-by** on the old ADR and **supersedes** on the new one.
4. Re-run this index generation if automated; otherwise update this table.
