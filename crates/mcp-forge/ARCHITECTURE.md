# liberado-mcp-forge — build MCP servers from git URLs

A small, run-and-exit CLI, not a daemon component. Its whole job: given a list of git URLs
(`mcp-sources.toml`), produce a runnable binary for each at the path
`McpTransport::Managed` resolution expects, and discard everything else `cargo` needed to get
there.

## Why this exists

Liberado's own MCP servers (`liberado-weather-mcp`, `liberado-pdf-mcp`, etc.) are plain Rust
crates on `turbomcp`. Building and wiring each one by hand into `topology.toml` — clone, `cargo
build --release`, copy the binary somewhere, hand-edit a `command` path — doesn't scale past a
couple of servers, and the file path drifts out of date the moment a server is rebuilt.

`cargo install --git <url> --root <dir> --locked` already does the hard part (clone, build
`--release` in a throwaway dir, copy just the binary to `<dir>/bin/`, discard the rest) — this
crate is a thin, sequential orchestrator around that, plus a build-skip cache so re-running is
cheap when nothing changed upstream.

## Convention over mutation

This tool never writes into `topology.toml`. `McpConfig.name`/`description`/`consequence` stay
human-authored (Liberado owns risk classification; MCPs don't self-declare it — see
`crates/common/src/config.rs`'s `McpConfig` doc comment). The only contract between this crate and
the daemon is [`liberado_common::config::managed_binary_path`]: given an install directory and a
name, both sides compute the same path independently. This crate's job ends at "make the binary
exist there"; `crates/bootstrap/src/lib.rs`'s `McpTransport::Managed` match arm is the read side.

## Pieces

- `sources.rs` — loads `mcp-sources.toml` (found via the same `liberado_config::config_dir()`
  the daemon uses, so it sits next to `topology.toml`). Each `[[source]]` has `name` (must match
  the `topology.toml` `[[mcps]]` entry it feeds), `git`, and two escape hatches for repos that
  don't follow the "binary name == repo name" convention: `package` (passed as `cargo install`'s
  trailing positional `CRATE` arg — it has no `-p`/`--package` flag — needed for Cargo virtual
  workspaces like `liberado-pdf-mcp`) and `bin` (`--bin` passthrough, for a package with more than
  one binary).
- `lock.rs` — `<install_dir>/.mcp-forge-lock.toml`, mapping `name -> last-built git SHA`.
  Co-located with the installed binaries (not the config dir), so wiping the install dir also
  invalidates the cache correctly instead of leaving a stale record behind.
- `build.rs` — for each source: `git ls-remote` the target rev, skip the build if the lockfile
  already matches (unless `--force`); otherwise run `cargo install --git ...`, then verify the
  binary landed at `managed_binary_path()` before recording success. A misconfigured `package`/
  `bin` override — or a repo that doesn't conform at all — surfaces here immediately, not later
  when the daemon tries and fails to spawn it.
- `main.rs` — `liberado-mcp-forge sync [--force] [--only <name>]`. One broken source doesn't abort
  the rest; the process exits non-zero if anything failed.

## Explicit non-goals (v1)

- HTTP/long-running managed servers — process supervision (start/health-check/restart-on-crash)
  is a real, separate daemon-lifecycle concern, not something this build tool does.
- Auto-syncing from the daemon on startup — stays a manual, separate step.
- `list`/`prune` subcommands — natural follow-ups, not needed yet.

## Dependencies

- `liberado-common` — `managed_binary_path()`, the shared path convention.
- `liberado-bootstrap` — `config_dir()` (where `mcp-sources.toml` lives), `mcp_install_dir()`
  (where binaries get installed).
- No `tokio` — a handful of sequential `cargo install` invocations doesn't need an async runtime.
- No `clap` — matches `liberado-cli`'s manual `std::env::args()` dispatch; the workspace has no
  arg-parsing library, and one flag pair (`--force`/`--only`) doesn't need one.
