# liberado-common — shared types (the vocabulary)

The foundation crate: pure data types and their invariants, **no logic, no I/O, no async**. Every
other crate speaks in these types, so they are the system's shared vocabulary. If a type is used by
two or more crates, it lives here.

## Modules

| Module | What it defines | Decision |
|---|---|---|
| `provenance` | `WriteProvenance` (`source` + `correlation_id` + zone/note), `PROVENANCE_KEY` (`_liberado_provenance`), `HUMAN_SOURCE`, `to_audit_metadata`/`from_audit_metadata`, `is_human()` | 5 |
| `capability` | `Zone`, `WriteClass`, `Capability`, `CapabilitySet` (narrow-only containment), `grants_mcp()` | 4 |
| `dispatch` | `DispatchDecision`, `DispatchAction` (`ExecuteDirect`/`DispatchSubagent`/`Clarify`), `Report`, `Outcome`, `BlockReason`, `ToolCall`, `ExecMode`, `JobHandle`/`JobStatus` | 1 |
| `event` | `Event`, `EventPayload`, `event_source` — one shape for **both** trigger paths (vault changes and hook webhooks) | 6 |
| `model` | `ModelProfile`, `ModelRole`, `ModelTier`, `ModelChoice`, `RequiredCaps` — role-tiered capability floors | 13 |
| `config` | `Config` (`Topology`/`Policy`/`Tuning`) + `validate()`; defaults live in code, config holds only deltas | 14 |
| `proposal` | `Proposal`, `ProposalStatus`, `ProposedAction` — the human-in-the-loop artifact written to `proposals/` | 11 |
| `error` | `Error`, `Result` | — |

## Key invariants

- **Provenance is best-effort, never a security boundary.** A missing/unrecognized provenance means
  "treat as external/unknown," never "trusted." Security is the capability/zone model.
- **`CapabilitySet` only narrows.** A subagent's capabilities are `base ∩ narrowing` — never widened
  (Decision 4). There is no API to grow a set.
- **`DispatchAction` is a typed, inspectable, loggable artifact**, not free prose — that is what makes
  safety engineered (deterministic guards run over this structure) rather than hoped-for.
- **Defaults in code, not config.** Every `Tuning` field's `Default` is its specced value, so an
  empty config still boots; `config` only carries overrides.

## Dependencies

- Depends on: only `serde`, `chrono`, `thiserror` (no internal crates).
- Depended on by: essentially everything (`vault`, `dispatcher`, `executor`, `mcp`, `orchestrator`,
  `daemon`, `provider-*` indirectly).

## Tests

Round-trip + invariant tests live in `tests/coverage.rs` and inline `#[cfg(test)]` modules.
