---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0019
open_items: false
---

# ADR-0019: TurboVault as Privileged Plugin, not Hard Dependency

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0019 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

TurboVault is a privileged default perception/storage plugin, not a hard dependency of the core dispatch/execute/MCP/chat path. Vault coupling is isolated to the reactive subsystem (watch + provenance loop-breaking) behind event-source traits. Mature pillars demote "vault is the only source of truth" to "vault is the default privileged plugin."

## Consequences

Core can compose without a vault. Reactive life-ops features still privilege TurboVault. Earlier decisions that said "vault is source of truth" are read through this clarification.

## Rejected alternatives

Hard-wiring every crate to TurboVault types. Treating vault absence as unsupported for the whole product surface.

## Implementation and tests

- `architecture/overview.md`
- `architecture/positioning.md`
- `../future-work/ideas/a2a-protocol-idea.md`
- `liberado-permissions-idea.md`
- `liberado-vault-concurrency-spec.md`
- `liberado-dispatch-logic-spec.md`
- `liberado-context-policy-spec.md`
- `liberado-inbox-spec.md`
- `liberado-vault-maintenance-and-git-spec.md`
- `liberado-config-spec.md`
- `liberado-testing-and-eval-spec.md`
- `liberado-conversation-store-spec.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: The original Pillar 1 ("the vault is the source of truth") read as a system-wide
invariant. The matured pillars demote it: the core must be usable without TurboVault, or the
"modular MCP/hook substrate" pillar and the general-MCP-agent milestone are not real.

**Decision**: **The vault (TurboVault) is the default, privileged perception+storage plugin, not a
hard dependency.** The core — dispatch / execute / MCP runtime / chat / conversation-store — is
**vault-agnostic**. The vault's coupling is isolated to the **reactive subsystem** (watch +
provenance loop-breaking), which becomes *the vault plugin* behind an **event-source / hook trait**
(the same trait cron implements — Decision 18). Privileged-default in the meantime: TurboVault stays
the out-of-the-box perception+storage layer, but nothing in the core path requires it. This is the
destination the mesh (Decision 18) reaches; vault-decoupling lands in roadmap Phase 3.

**Supersedes**: the earlier framing that "the vault is the source of truth" as a system-wide
invariant (see the dated clarifying note on Decision 17 above). Pillar 1 in
[`architecture/overview.md`](../spec/architecture/overview.md) now reads "vault = default
perception+storage plugin"; [`architecture/positioning.md`](../spec/architecture/positioning.md)
states the differentiation this unlocks.

**Status**: Decided (2026-06-26). ? **Event-source trait built (2026-07-04)** — the vault's reactive
coupling is now isolated behind `EventSource` (`liberado-common`), with `liberado-cron` as the proof
a non-vault source works identically. The vault itself (`liberado-vault`) is unchanged and still the
default, privileged perception+storage plugin — this decision was about isolating the *coupling*,
not removing the vault.

---

## Tier 4: Lower-Regret / Polish Decisions

- **A2A (Agent2Agent) protocol interop** — not yet a decision, captured as
  [`a2a-protocol-idea.md`](../future-work/ideas/a2a-protocol-idea.md) (2026-07-01). Preliminary read: the
  Decision 17 conversation-store seams (`author`, lineage) and the Decision 18 mesh direction
  already carry most of the data-model need; the open gap is a new inbound protocol surface and
  an outbound peer-delegation capability, gated like any other MCP/subagent trust boundary. Not
  before Phase 3.
- Exact initial model/provider and SDK choice (DeepSeek route, config approach).
- ~~Precise naming for the enhanced liberado component~~ **Resolved**: split into `liberado-dispatcher` (new out-of-band routing agent) + `liberado-memory-mcp` (renamed `liberado-tool-helper-mcp`, the mem0-backed general + procedural memory store the dispatcher consumes). Actual directory rename happens at implementation time (planning phase keeps the existing folder name).
- v1 scope boundaries (what is explicitly deferred).
- Documentation location for system prompts and dispatch logic (vault vs code).

---

## Next Actions

**All decisions resolved — Tier 1 (1–5), Tier 2 (6–12), Tier 3 (13–16), Decision 17, the matured-vision
mesh/modularity decisions (18–19, 2026-06-26), and the Tier 4 naming item.**

Companion specs:
- `liberado-permissions-idea.md` — Decision 4 (capability/zone model)
- `liberado-vault-concurrency-spec.md` — Decision 5 (provenance, loop-breaking)
- `liberado-dispatch-logic-spec.md` — Decision 1 (routing) + Decisions 8 interfaces
- `liberado-context-policy-spec.md` — main-agent context (deliberately dumb header)
- `liberado-inbox-spec.md` — async capture + ambient analysis
- `liberado-vault-maintenance-and-git-spec.md` — git backstop + maintenance tasks
- `liberado-config-spec.md` — Decision 14 (config topology)
- `liberado-testing-and-eval-spec.md` — Decision 16 (integration-test harness)
- `liberado-conversation-store-spec.md` — Decision 17 (conversation history store)

Remaining Tier 4 (lower-regret, can settle during implementation): exact initial model/provider + SDK choice; v1 scope boundaries; doc location for system prompts.

These two steps are realized (June 24, 2026):
1. **Core shared types** — `crates/common` holds the full type vocabulary (provenance, capability, dispatch, event, model, config, proposal).
2. **V1 vertical slice** — The daemon?dispatcher?orchestrator?executor pipeline is end-to-end wired, tested, and the proposal approve?execute loop is closed.

This log is updated after each decision is resolved.
