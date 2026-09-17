---
kind: reference
status: active
authority: normative
domain: build
canonical_for: life-os-build
open_items: false
last_verified: 2026-09-17
---

# Life OS build profile

The workspace contains every crate Liberado ships, including sidecars the Life OS
product root does not run. The `just build` recipe compiles them all; this doc
defines a narrower profile for a Life OS-shaped build.

## Recipes

| Recipe | What it builds |
|---|---|
| `just build` | Full native workspace. CI, mutation campaign, `cargo test --workspace`. |
| `just build-release` | Release binary of `-p liberado-cli` (the `liberado` binary). Already Life OS-shaped — the package's dep graph is the Life OS graph. |
| `just build-life-os` | Debug build of every workspace member that is a Life OS product crate. Excludes the sidecars in the next section. |
| `just build-life-os-release` | Release build of the same Life OS set, e.g. for shipping `liberado-conformance`. |

The Dockerfile is unchanged: it already builds `-p liberado-cli
-p liberado-conformance`, which is exactly the Life OS set.

## Gated packages

These crates stay in the workspace (CI still compiles them via `just build`)
but are fenced out of `build-life-os`:

- `liberado-sysmap` — the 2D system-map library.
- `liberado-sysmap-cli` — the `just sysmap` / `just sysmap-json` runner.
- `liberado-sysmap-gui` — the native window.
- `sysmap-core` — graph model shared by the sysmap crates.
- `liberado-provider-free-proxy` — free-proxy provider binary.

None of these are runtime dependencies of `liberado-cli`, `liberado-server`,
`liberado-daemon`, or `liberado-main-agent`. The `Runtime wiring for the 2D
system map` comments in those crates' `Cargo.toml` files are
`[package.metadata.liberado.flows]` entries for the sysmap tool — not Cargo
dependencies — and must remain.

`liberado-webui` is also excluded. The WebUI is WASM-only; `cargo build` on the
host produces no useful artifact and the workspace Clippy already excludes it.

## Why the crates stay in the workspace

CI must keep building everything (`cargo build --workspace`,
`cargo test --workspace`). Removing the crates from `[workspace].members`
would break that contract and every docs/impact binding. Excluding them on the
build command keeps CI whole and gives Life OS a fast path.

## Operator commands

```console
# A: today's full workspace build (unchanged)
just build
# equivalent cargo:
cargo build --locked --workspace

# B: Life OS build profile (debug)
just build-life-os
# equivalent cargo:
cargo build --locked --workspace \
    --exclude liberado-webui \
    --exclude liberado-sysmap \
    --exclude liberado-sysmap-cli \
    --exclude liberado-sysmap-gui \
    --exclude sysmap-core \
    --exclude liberado-provider-free-proxy
```

The release binary stays `just build-release` regardless of profile — the
`liberado` binary is built from `-p liberado-cli`, which never pulled in the
gated crates to begin with.
