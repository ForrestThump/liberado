# Phase 4: Docker MCP transport (`ExecutionEnvironment`, v1 slice)

**Status: done (2026-07-07).**

## Why

`docs/roadmap.md`'s Phase 4 named "an `ExecutionEnvironment` trait (Local / Docker /
serverless hibernation)" as the last remaining gap against this project's own competitive thesis
(`docs/spec/architecture/positioning.md`): Hermes is only "still ahead" until Liberado closes
self-improvement (done, `riggers`), cron (done), subagents (mostly done), and execution environments
(this slice). Before this, every `topology.toml`-declared MCP either ran as a local child process
(`McpTransport::Stdio`) or was a remote HTTP server (`McpTransport::Http`) — no way to run an MCP
server in an isolated container, and no isolation story for a freshly-scaffolded,
not-yet-human-reviewed MCP that `riggers` (the self-improvement engine) might produce.

## Why not an `ExecutionEnvironment` trait in the executor crate

Confirmed by reading the actual source, not assumed: `crates/executor`'s own `ARCHITECTURE.md`
states it is MCP-agnostic and owns no transport/connection concerns — a Hermes-style
`ExecutionEnvironment` trait *in the executor crate* would be the wrong layer for this codebase.
Hermes' agent runs raw shell commands directly in a chosen backend; Liberado's agent only ever calls
capability-gated MCP tools through `ToolRuntime`, so "which environment" here really means "where
does the MCP *server* process live" — a `crates/mcp`/`crates/bootstrap` connector concern, not an
executor one. The existing `McpConnector` trait (`crates/mcp/src/connector.rs`) with its
`StdioConnector`/`HttpConnector` implementations was already the right abstraction boundary; this
extends it rather than inventing a new one.

Also confirmed: `riggers/Dockerfile` already containerizes the self-improvement MCP and its `vtcode`
coding-agent binary, but as an operator-managed, externally-deployed long-running service (wired via
plain `transport = { kind = "stdio", command = "riggers-mcp", ... }` in `topology.toml`) — not
something Liberado's own connector layer spawns or owns. This slice builds the generic mechanism
(Liberado can spawn an MCP inside a container on demand); wiring `riggers` itself through it for
per-task ephemeral sandboxing is a valuable but explicitly separate follow-on, out of scope here.

**Deliberately deferred, not built**: serverless hibernation (Modal/Daytona-style spin-to-zero) — no
MCP today has an idle-cost problem that justifies the real cloud-backend integration cost this would
require. Revisit only once a concrete MCP needs it.

## Design

### The core bet: reuse `StdioConnector` unchanged, don't invent a new connector type

MCP-over-stdio doesn't care whether the process on the other end of the pipe is a bare binary or
`docker run -i --rm image ...` — both are just a child process with piped stdin/stdout. Verified the
lifecycle concern directly: `turbomcp-transport`'s `ChildProcessTransport` has `kill_on_drop: true`
by default (`impl Drop` calls `child.start_kill()`), and its async `stop_process()` path additionally
drops the stdin/stdout channels, aborts IO tasks, then waits up to `shutdown_timeout` (10s default).
When the child is `docker run -i --rm ...`: killing that process breaks the attached stdin pipe →
Docker sends EOF to the container → a correctly-implemented MCP server (all turbomcp-based servers
qualify) exits on stdin-close → the container exits → `--rm` removes it. No explicit `docker stop`
call, no container-ID tracking, no new connector type needed. The only failure mode `--rm` doesn't
cover is a badly-behaved server that ignores stdin-close and hangs forever — the same failure mode
any hung stdio MCP already has today, not a new risk this design introduces.

### `crates/config-loader/src/model.rs` — `McpTransport::Docker`

```rust
Docker {
    image: String,
    command: Option<String>,   // None = use the image's own CMD/ENTRYPOINT
    args: Vec<String>,
    volumes: Vec<String>,      // Docker CLI format: "host:container" or "host:container:ro"
    env: Vec<String>,          // "KEY=value", or bare "KEY" to pass through from the host env
},
```

`command: Option<String>` so an image with a sensible built-in `CMD` doesn't force a redundant
override. `volumes`/`env` as `Vec<String>` in Docker CLI syntax (not `Vec<(String, String)>`
tuples) — TOML serializes tuples as arrays-of-arrays, unfamiliar; the CLI-format strings map
directly to `--volume`/`--env` flags and naturally support the passthrough-from-host-env pattern
(`"API_KEY"` with no `=value` → `--env API_KEY`, letting a secret live only in the environment,
never in `topology.toml` — Decision 10). `Config::validate()` rejects a blank `image`, mirroring the
existing `cron_expr`-blank-rejection pattern for `topology.schedules`.

### `crates/bootstrap/src/lib.rs` — the actual wiring point

A private `docker_argv(image, command, args, volumes, env) -> Vec<String>` pure function (same
"pull logic out for isolated testability" pattern as `resolve_fire_at`/`resolve_config_dir`
elsewhere in this codebase) builds `["run", "-i", "--rm", "--volume", ..., "--env", ..., image,
command?, args...]` — deliberately **no `-t`** (pseudo-TTY), which would insert `\r` into every line
and corrupt the newline-delimited JSON-RPC stream. `mcp_registry_from_config`'s existing `match`
gained a `McpTransport::Docker { .. }` arm that calls `docker_argv` and hands the result straight to
`StdioConnector::new("docker", argv)` — no dedicated connector type.

Note: `mcp_registry_from_config` is already called once per pool (the default pool plus each
additional named pool in `configure_daemon`'s fold) — a Docker MCP enabled in two pools means two
independent `docker run -i --rm` processes while both are active. Correct behavior (each pool needs
its own connection, same as any other transport), just worth knowing.

### Platform notes

Development happens on Windows; the actual deployment target is Debian, where none of this applies —
these are dev-machine caveats, not production concerns:

- **Volume paths need forward slashes on Windows**: Docker Desktop's WSL2 backend accepts
  `C:/Users/...:/vault` but not reliably `C:\Users\...`. A native Linux Docker host takes forward
  slashes naturally (it's just a real filesystem path), so this is a Windows-only reminder.
- **Pre-pull images before enabling on Windows**: first `docker run` can hit the connect timeout if
  the WSL2 VM needs to wake from idle *and* the image needs pulling. No such VM-wake latency on a
  native Linux Docker daemon.

## Files changed

- `crates/config-loader/src/model.rs` — `Docker` variant, validation, 4 new tests.
- `crates/bootstrap/src/lib.rs` — `docker_argv` + the new `match` arm, 7 new tests.
- `config.example/topology.toml` — commented example.
- `crates/mcp/ARCHITECTURE.md` — connector section updated (also fixed pre-existing staleness: it
  described a `ClientConnector`/`TurbomcpRuntimeFactory<C>` shape that no longer matches the actual
  `McpConnector`/`McpRegistry` types).
- Unchanged: `crates/mcp/src/connector.rs` (no new connector type), `crates/executor/*` (confirmed
  not the right layer).

## Verification

- `cargo build --workspace` / `cargo clippy --all-targets` clean.
- Unit tests: `docker_argv` (image-only; with command+args; with volumes; with env; all combined),
  config round-trip tests for the new TOML shape (minimal and fully-populated), a validation test
  rejecting a blank `image`, and a `mcp_registry_from_config` test confirming a Docker-transport MCP
  registers under its name.
- Live end-to-end smoke test: see `docs/future-work/archive/human-todo.md` — needs Docker Desktop running on
  this Windows dev machine, which this session doesn't have set up yet.
