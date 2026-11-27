# liberado-common — shared types (the vocabulary)

The foundation crate: shared data types and their invariants, plus a small, focused set of I/O
helpers that other crates need. Most of the crate is types and pure functions. The I/O is narrow
and deliberate: `approval_ledger` reads and writes an approval file through `tokio::fs`, `process`
launches bounded subprocesses, and `path` wraps `std::fs::canonicalize` behaviour. The crate
depends on `tokio`, so it also carries async runtime work. Every other crate speaks in these
types, so they are the system's shared vocabulary. If a type is used by two or more crates, it
lives here.

## Modules

### Vocabulary subset

The rows below cover the types other crates name most often. The full inventory follows.

| Module | What it defines | Decision |
|---|---|---|
| `provenance` | `WriteProvenance` (`source` + `correlation_id` + zone/note), `PROVENANCE_KEY` (`_liberado_provenance`), `HUMAN_SOURCE`, `to_audit_metadata`/`from_audit_metadata`, `is_human()` | 5 |
| `capability` | `Zone`, `WriteClass`, `Capability`, `CapabilitySet` (narrow-only containment), `grants_mcp()`, `grants_ask_human()`, `Consequence`, `CONSEQUENCE_GATE`, `instruction_scope()` + the magnitude heuristics | 4 |
| `catalog` | `CapabilityCatalog`, `McpDescriptor` — the live, thread-safe (`Arc<RwLock<_>>` + a `watch` channel for change notification) runtime registry of available MCPs, populated at boot from `topology.mcps` and updated as MCPs come and go. The one shared `Arc<CapabilityCatalog>` the dispatcher, daemon, chat, and API all route against. | — |
| `dispatch` | `DispatchDecision`, `DispatchAction` (`ExecuteDirect`/`DispatchSubagent`/`Clarify`/`Propose`), `Delivery`, `Depth`, `Report`, `Outcome`, `BlockReason`, `ToolCall`, `ExecMode`, `JobHandle`/`JobStatus` | 1 |
| `event` | `Event`, `EventPayload`, `event_source` — one shape for **both** trigger paths (vault changes and hook webhooks) | 6 |
| `local_time` | `UserTimezone`, `DEFAULT_TIMEZONE` — IANA zone + helpers to stamp "Local time: …" onto agent context when a caller opts in (config SSoT is `topology.timezone`) | — |
| `model` | `ModelProfile`, `ModelRole`, `ModelTier`, `ModelChoice`, `RequiredCaps` — role-tiered capability floors | 13 |
| `proposal` | `Proposal`, `ProposalStatus`, `ProposedAction` — the human-in-the-loop artifact written to `proposals/` | 11 |
| `error` | `Error`, `Result` | — |

### Full inventory

`crates/common/src/lib.rs` declares 25 modules: 22 public, 3 private.

| Module | Visibility |
|---|---|
| `approval_ledger` | public |
| `bundled_vault` | private |
| `capability` | public |
| `catalog` | public |
| `civil_schedule` | public |
| `clock` | public |
| `permission` | public |
| `write_target` | private |
| `dispatch` | public |
| `error` | public |
| `event` | public |
| `failure_excerpt` | public |
| `frontmatter` | public |
| `guidance` | public |
| `local_time` | public |
| `model` | public |
| `path` | public |
| `process` | public |
| `proposal` | public |
| `provenance` | public |
| `redact` | public |
| `risk_waiver` | public |
| `session_grants` | public |
| `sweeping` | private |
| `utc_stamp` | public |

The typed config model (`Config`/`Topology`/`Policy`/`Tuning`) used to live here as a `config`
module — moved to `liberado-config-loader` 2026-07-04 (`docs/future-work/archive/hygiene-audit-2026-07-04.md`,
re-exported from `liberado-config`) to avoid a dependency cycle: `liberado-config-loader`'s own
cross-cutting validation needs the type, and `liberado-config` already depends on
`liberado-config-loader`. Nothing in this crate reaches for it, so it doesn't belong in the shared
vocabulary every crate compiles against regardless of whether it touches config.

## Two constants that live here because two guards must agree

- **`CONSEQUENCE_GATE`** was private to `liberado-dispatcher`. It is now read by two independent
  guards — the dispatcher's (may this run without asking?) and the orchestrator's delivery guard
  (may this report bypass the main agent?). Two copies drifting apart would mean an action safe
  enough to run unattended is simultaneously too dangerous to report directly, or the reverse.
- **`INSTRUCTION_SCAN_LIMIT`** caps how much of a goal the magnitude heuristics read, because a goal
  carrying pasted content is an instruction *plus* a document. Both the pre-flight guard and
  `RiskGatedToolRuntime` now read a goal through `instruction_scope`; previously only one did, so the
  same goal could pass one guard and trip the other.

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

- Depends on: no internal crates. External: `async-trait`, `serde`, `serde_json`, `serde_norway`,
  `jiff`, `thiserror`, `tokio`, `toml`, `hmac`, `sha2`, `rand`.
- Depended on by: essentially everything (`vault`, `dispatcher`, `executor`, `mcp`, `orchestrator`,
  `daemon`, `provider-*` indirectly).

## Tests

Round-trip + invariant tests live in `tests/coverage.rs` and inline `#[cfg(test)]` modules.
