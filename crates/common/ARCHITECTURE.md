# liberado-common — shared types (the vocabulary)

The foundation crate: pure data types and their invariants, **no logic, no I/O, no async**. Every
other crate speaks in these types, so they are the system's shared vocabulary. If a type is used by
two or more crates, it lives here.

## Modules

| Module | What it defines | Decision |
|---|---|---|
| `provenance` | `WriteProvenance` (`source` + `correlation_id` + zone/note), `PROVENANCE_KEY` (`_liberado_provenance`), `HUMAN_SOURCE`, `to_audit_metadata`/`from_audit_metadata`, `is_human()` | 5 |
| `capability` | `Zone`, `WriteClass`, `Capability`, `CapabilitySet` (narrow-only containment), `grants_mcp()` | 4 |
| `catalog` | `CapabilityCatalog`, `McpDescriptor` — the live, thread-safe (`Arc<RwLock<_>>` + a `watch` channel for change notification) runtime registry of available MCPs, populated at boot from `topology.mcps` and updated as MCPs come and go. The one shared `Arc<CapabilityCatalog>` the dispatcher, daemon, chat, and API all route against. | — |
| `dispatch` | `DispatchDecision`, `DispatchAction` (`ExecuteDirect`/`DispatchSubagent`/`Clarify`), `Report`, `Outcome`, `BlockReason`, `ToolCall`, `ExecMode`, `JobHandle`/`JobStatus` | 1 |
| `event` | `Event`, `EventPayload`, `event_source` — one shape for **both** trigger paths (vault changes and hook webhooks) | 6 |
| `model` | `ModelProfile`, `ModelRole`, `ModelTier`, `ModelChoice`, `RequiredCaps` — role-tiered capability floors | 13 |
| `proposal` | `Proposal`, `ProposalStatus`, `ProposedAction` — the human-in-the-loop artifact written to `proposals/` | 11 |
| `error` | `Error`, `Result` | — |

The typed config model (`Config`/`Topology`/`Policy`/`Tuning`) used to live here as a `config`
module — moved to `liberado-config-loader` 2026-07-04 (`docs/roadmap/hygiene-audit-2026-07-04.md`,
re-exported from `liberado-config`) to avoid a dependency cycle: `liberado-config-loader`'s own
cross-cutting validation needs the type, and `liberado-config` already depends on
`liberado-config-loader`. Nothing in this crate reaches for it, so it doesn't belong in the shared
vocabulary every crate compiles against regardless of whether it touches config.

## Key invariants

- **Provenance is best-effort, never a security boundary.** A missing/unrecognized provenance means
  "treat as external/unknown," never "trusted." Security is the capability/zone model.
- **`CapabilitySet` only narrows.** A subagent's capabilities are `base ∩ narrowing` — never widened
  (Decision 4). There is no API to grow a set.
- **`DispatchAction` is a typed, inspectable, loggable artifact**, not free prose — that is what makes
  safety engineered (deterministic guards run over this structure) rather than hoped-for.
- **Defaults in code, not config.** Every `Tuning` field's `Default` is its specced value, so an
  empty config still boots; the config model (`liberado-config-loader`) only carries overrides.

## Dependencies

- Depends on: only `serde`, `chrono`, `thiserror` (no internal crates).
- Depended on by: essentially everything (`vault`, `dispatcher`, `executor`, `mcp`, `orchestrator`,
  `daemon`, `provider-*` indirectly).

## Tests

Round-trip + invariant tests live in `tests/coverage.rs` and inline `#[cfg(test)]` modules.
