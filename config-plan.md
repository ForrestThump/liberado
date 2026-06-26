# Configuration Architecture Plan — Liberado

Status: Draft (no implementation yet)  
Date: 2026-06-26  
Owner: (to be assigned)

## Goal
Design a configuration system that supports a **mesh / loosely-coupled** architecture:
- Individual crates can ship with their own sensible `config/` examples or local defaults.
- A single workspace-level `config/` directory provides unified, overrideable settings.
- Developers, deployers, and downstream projects can choose the granularity they need without tight coupling.

## Current State (baseline)
- Config lives at `dirs::config_dir()/liberado` (Windows: `%APPDATA%\liberado`) or `LIBERADO_CONFIG_DIR`.
- Three optional files: `topology.toml`, `policy.toml`, `tuning.toml`.
- `config.example/` in the repo root contains starter files.
- `liberado config check` validates whatever directory is resolved.
- Environment variable `LIBERADO_CONFIG_DIR` gives full override power.

## Proposed Layout
```
life-os/
├── config/                      # workspace master (highest precedence when present)
│   ├── topology.toml
│   ├── policy.toml
│   └── tuning.toml
├── crates/
│   ├── cli/
│   │   └── config/              # crate-local examples / defaults
│   ├── daemon/
│   │   └── config/
│   ├── dispatcher/
│   │   └── config/
│   └── ...
└── config.example/              # (kept or merged into per-crate + root examples)
```

## Precedence & Resolution Order
1. `LIBERADO_CONFIG_DIR` (explicit full override) — if set and non-empty, use exactly this directory. Stop.
2. Root `config/` (if directory exists in workspace root) — treated as the unified workspace config.
3. Falling back, each crate may look in its own `config/` sibling directory for crate-specific values (used when the crate runs stand-alone or in tests).
4. Built-in defaults inside the crate (current `Default` impls).

Result: downstream binary users still get the classic `%APPDATA%\liberado` behavior unless they set the env var. Workspace developers gain `config/` at the root. Individual crates keep autonomy.

## Environment Variables
- `LIBERADO_CONFIG_DIR` — unchanged semantics (highest precedence).
- Optional new var per crate (e.g. `LIBERADO_DAEMON_CONFIG_DIR`) — only if a crate demonstrates a genuine need for independent override; otherwise keep surface small.

## Discovery Rules
- A directory is only considered a config source if it contains at least one of the three recognized files (`topology.toml` etc.). Empty directories are ignored.
- Root `config/` wins over any per-crate `config/` when both would apply to the same process.
- Per-crate `config/` is primarily intended for:
  - crate-local example files
  - unit / integration test fixtures
  - crates used as libraries in other projects

## CLI / Tooling Impact
- `liberado config check` should:
  - Report which directory it resolved and why (env var, root/config, per-crate, or none).
  - Support a `--crate <name>` flag to validate a single crate's local config in isolation.
- `cargo test` inside a crate should automatically pick up that crate's `config/` for any test-only wiring.

## Migration & Back-compat
- Existing users with config in `%APPDATA%\liberado` are unaffected.
- Workspace developers can copy `config.example/` into root `config/` and/or per-crate folders.
- `config.example/` may be deprecated in favor of the new root + per-crate locations.

## Risks & Mitigations
- Confusion about “which config wins” → clear precedence list + improved `config check` output.
- Accidental shipping of dev configs → `.gitignore` the root `config/` (same as today for `.liberado/`).
- Over-engineering for crates that never need separate config → make per-crate folders strictly opt-in; crates without a `config/` dir simply fall through.

## Open Questions
- Do we want a small `config-loader` crate that both the bootstrap and individual crates can depend on?
- Should `config check` also validate cross-references between root and per-crate files (e.g., an MCP referenced in root topology but defined only in a crate)?
- Naming: keep `config/` everywhere, or use `examples/config/` for non-active files?

## Next Steps (when approved)
1. Write the plan into AGENTS.md or a dedicated decision record.
2. Implement a thin resolver in `bootstrap` that follows the precedence above.
3. Update `config check` output and docs.
4. Seed root `config/` and one or two example per-crate configs.
5. Remove or redirect `config.example/`.

This plan keeps the system simple for end-users while giving crate authors the autonomy needed for a healthy mesh architecture.
