# liberado-mcp-forge — build MCP servers from git URLs or local paths

A small, run-and-exit CLI, not a daemon component. Its whole job: given a list of sources
(`mcp-sources.toml` — each either a git URL or a local directory), produce a runnable binary for
each at the path `McpTransport::Managed` resolution expects, and discard everything else `cargo`
needed to get there.

## Why this exists

Liberado's own MCP servers (`liberado-weather-mcp`, `liberado-pdf-mcp`, etc.) are plain Rust
crates on `turbomcp`. Building and wiring each one by hand into `topology.toml` — clone, `cargo
build --release`, copy the binary somewhere, hand-edit a `command` path — doesn't scale past a
couple of servers, and the file path drifts out of date the moment a server is rebuilt.

`cargo install --git <url> --root <dir> --locked` already does the hard part (clone, build
`--release` in a throwaway dir, copy just the binary to `<dir>/bin/`, discard the rest) — this
crate is a thin, sequential orchestrator around that, plus a build-skip cache so re-running is
cheap when nothing changed upstream.

A second, later addition: `cargo install --path <dir>` for sources that can't be built from an
isolated git clone at all — a co-developed MCP with a local path dependency on this workspace's own
crates (or a fork's local checkout), like `liberado-deliberate-mcp` and `riggers`. Those existed
before this tool could build them and were wired by hand into `topology.toml`'s `command` field
instead — the same error-prone manual-path problem this whole tool exists to avoid (concretely: a
config-directory mixup this session, caught only by a live smoke test). `path` sources close that
gap without needing them to stop being path-coupled — see "Two kinds of source" below.

## Convention over mutation

This tool never writes into `topology.toml`. `McpConfig.name`/`description`/`consequence` stay
human-authored (Liberado owns risk classification; MCPs don't self-declare it — see
`crates/config-loader/src/model.rs`'s `McpConfig` doc comment). The only contract between this crate and
the daemon is [`liberado_config::managed_binary_path`]: given an install directory and a
name, both sides compute the same path independently. This crate's job ends at "make the binary
exist there"; `crates/bootstrap/src/lib.rs`'s `McpTransport::Managed` match arm is the read side.

## Two kinds of source

Every `[[source]]` in `mcp-sources.toml` declares exactly one of `git`/`path` (validated in
`sources.rs`, not by TOML's type system — it has no clean tagged-union for "exactly one of these"):

- **`git`** — the original, still-preferred shape for a genuinely standalone repo. Built via
  `cargo install --git <url>` in cargo's own isolated clone; skips the rebuild if `git ls-remote`'s
  resolved SHA already matches the lockfile.
- **`path`** — for a source that can't be built from an isolated clone at all, because it has a
  local path dependency on something unpublished (this workspace's own crates, or a fork's local
  checkout — `liberado-deliberate-mcp`'s dependency on `../crates/*`, or `liberado-wakeup-mcp`'s
  now-unnecessary-but-previously-considered dependency on a local `turbomcp` checkout, are exactly
  this shape). Built via `cargo install --path <dir>` instead. There's no remote ref to check
  against a lockfile, so a `path` source **always rebuilds** on `sync` — cargo's own incremental
  cache already keeps a no-op rebuild cheap, so tracking "did anything actually change" ourselves
  isn't worth the complexity the way it is for a network fetch.

One consequence worth knowing: `--git`'s package-selection for a Cargo workspace uses a trailing
positional `CRATE` argument (`cargo install` has no `-p`/`--package` flag for a git/registry
install); `--path`'s package selection uses the real `-p`/`--package` flag instead — a real cargo
quirk, not a choice made here. `sources.rs`'s `McpSource::package` doc comment covers both.

## Pieces

- `sources.rs` — loads `mcp-sources.toml` (found via the same `liberado_config::config_dir()`
  the daemon uses, so it sits next to `topology.toml`). Each `[[source]]` has `name` (must match
  the `topology.toml` `[[mcps]]` entry it feeds), exactly one of `git`/`path`, and two escape
  hatches for sources that don't follow the "binary name == repo/dir name" convention: `package`
  (see "Two kinds of source" above for how its meaning differs by source kind — needed for Cargo
  virtual workspaces like `liberado-pdf-mcp`) and `bin` (`--bin` passthrough, for a package with
  more than one binary).
- `lock.rs` — `<install_dir>/.mcp-forge-lock.toml`, mapping `name -> last-built version` (a git SHA
  for `git` sources; `path` sources never get an entry, since they always rebuild). Co-located with
  the installed binaries (not the config dir), so wiping the install dir also invalidates the cache
  correctly instead of leaving a stale record behind.
- `build.rs` — for a `git` source: `git ls-remote` the target rev, skip the build if the lockfile
  already matches (unless `--force`); for a `path` source: no version check, always build. Either
  way: run the right `cargo install` invocation, then verify the binary landed at
  `managed_binary_path()` before recording success. A misconfigured `package`/`bin` override — or a
  source that doesn't conform at all — surfaces here immediately, not later when the daemon tries
  and fails to spawn it.
- `main.rs` — `liberado-mcp-forge sync [--force] [--only <name>]`. One broken source doesn't abort
  the rest; the process exits non-zero if anything failed.

## Explicit non-goals (v1)

- HTTP/long-running managed servers — process supervision (start/health-check/restart-on-crash)
  is a real, separate daemon-lifecycle concern, not something this build tool does.
- Auto-syncing from the daemon on startup — stays a manual, separate step.
- `list`/`prune` subcommands — natural follow-ups, not needed yet.
- Watching a `path` source for changes and rebuilding automatically — `sync` is still an explicit,
  manual step; `path` only changes *what* gets built from, not *when*.

## Dependencies

- `liberado-common` — `managed_binary_path()`, the shared path convention.
- `liberado-config` — `config_dir()` (where `mcp-sources.toml` lives), `mcp_install_dir()`
  (where binaries get installed). Deliberately depends on `liberado-config` directly, not
  `liberado-bootstrap` — the config/path resolution helpers were factored out specifically so a
  build tool like this one isn't forced to pull in the whole provider/dispatcher/orchestrator
  assembly stack just to find a directory.
- No `tokio` — a handful of sequential `cargo install` invocations doesn't need an async runtime.
- No `clap` — matches `liberado-cli`'s manual `std::env::args()` dispatch; the workspace has no
  arg-parsing library, and one flag pair (`--force`/`--only`) doesn't need one.
