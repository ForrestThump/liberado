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

## Precedence & Resolution Order (per-file overlay)
For **each** of `topology.toml`, `policy.toml`, `tuning.toml` independently:

1. Start from built-in `Default`.
2. Overlay `LIBERADO_CONFIG_DIR/<file>` (if the env var is set **and** the file exists there).
3. Overlay root `config/<file>` (if present).
4. Overlay `<crate>/config/<file>` (only for crate-local examples via compile-time `CARGO_MANIFEST_DIR`; runtime per-crate overrides require an explicit env var or absolute path).

Higher layers win at the TOML table/key level. `LIBERADO_CONFIG_DIR` is now a **base directory**, not a short-circuit.

Result: partial per-file overrides work naturally. Downstream binary users retain `%APPDATA%\liberado` fallback. Workspace and crate authors gain controlled layering without tight coupling.

## Environment Variables
- `LIBERADO_CONFIG_DIR` — directory that can supply any of the three files (overlay, not stop).
- Per-crate runtime overrides: use an explicit absolute path or a second env var only when genuinely required; otherwise rely on root `config/`.

## Discovery Rules
- Only directories containing at least one recognized file are sources.
- Root `config/` and `LIBERADO_CONFIG_DIR` are runtime sources.
- Per-crate `config/` is **compile-time only** (via `env!("CARGO_MANIFEST_DIR")`) for examples; runtime per-crate config always requires an explicit path.

## CLI / Tooling Impact
- `liberado config check` reports the exact source file for every loaded value (provenance) and validates the **merged** result only.
- `--crate <name>` flag is dropped until crate-config discovery is designed.
- Tests construct `Config` via `Config::from_str` / builder in `common`; filesystem fixtures are only for loader integration tests.

## Migration & Back-compat
- Existing users unchanged.
- `config.example/` remains the committed starter location; root `config/` and per-crate `config/` hold only local overrides (git-ignored).

## Risks & Mitigations
- Confusion about “which config wins” → per-file provenance in `config check` output.
- Accidental shipping of dev configs → explicit `.gitignore` rules (see table below).
- Over-engineering → per-crate runtime config strictly opt-in via explicit path; examples remain compile-time only.

## `.gitignore` Policy

| Path                        | Git status     | Purpose                          |
|-----------------------------|----------------|----------------------------------|
| `config.example/`           | committed      | Starter files                    |
| `config/*.toml`             | `.gitignore`   | Local deployment overrides       |
| `crates/*/config.example/`  | committed      | Per-crate examples               |
| `crates/*/config/*.toml`    | `.gitignore`   | Per-crate deployment overrides   |

## Decisions (closed)
- `config-loader` crate: **yes** — thin `ConfigSource` trait + `ChainLoader` in a new crate (`liberado-config-loader`).
- Cross-reference validation: **validate merged config only**, never per-directory.
- Naming: keep `config/` for runtime, `config.example/` for committed starters.

## Next Steps (approved)
1. Add `schema_version` (optional) to `tuning.toml` + deprecation warning path in loader.
2. Create `liberado-config-loader` crate with `ConfigSource` trait + `ChainLoader`.
3. Move merged-config validation into the loader crate.
4. Add `Config::from_str` / `Config::builder` to `common`.
5. Update `config check` to emit per-value provenance.
6. Update `.gitignore` with explicit table (committed vs. local).
7. Document compile-time example paths via `CARGO_MANIFEST_DIR`.
8. Write plan into AGENTS.md decision record.

This revised plan eliminates the previous critical and medium-severity issues while preserving mesh autonomy.
