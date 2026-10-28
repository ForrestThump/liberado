# liberado-mcp — the MCP integration layer (executor ⇄ real MCP tools)

Implements the executor's `ToolRuntime` over `turbomcp-client`, so the agent loop can drive a real
MCP server's tools, and provides the **factory** that the orchestrator uses to obtain those runtimes
on demand. This is the crate that makes tool calls *actually happen* — and the crate that makes
tool-mediated vault writes **self-attributed** (loop-broken).

Three pieces:
- `TurbomcpRuntime` — the `ToolRuntime` itself (below).
- `McpConnector` + `StdioConnector`/`HttpConnector` — how to obtain a connected client (`connector.rs`).
- `McpRegistry` — the `RuntimeFactory` the orchestrator depends on (`factory.rs`).

## `TurbomcpRuntime<T: Transport>`

Constructed per execution via `connect(client, provenance)`:

1. **Catalog** — `initialize()` then `list_tools()`; each MCP `Tool` is mapped to the provider's
   `ToolDef` (its `input_schema` serialized to JSON-Schema `parameters`). Cached, because
   `ToolRuntime::catalog()` is synchronous.
2. **Invoke** — runs the model-requested tool via `call_tool_with_meta`, flattens `CallToolResult`
   → string (text blocks, or structured content), and surfaces an `isError` result as an in-band
   `Err` the executor feeds back to the model.
3. **Provenance** — the runtime carries one `WriteProvenance` for its lifetime and injects it into
   **every** call's `_meta` (as `{ "_liberado_provenance": … }`).

## Why the `_meta` injection matters

This is the payoff of the whole turbomcp/turbovault `_meta` plumbing. When a tool writes to the
vault, the write's audit entry carries our provenance; the daemon's `attribute()` then recognizes the
resulting change as ours and suppresses it. Without it, every agent write would trigger a reaction →
infinite loop. The mechanism is proven end-to-end in `liberado-vault`'s `provenance_e2e`.

## The registry + connectors

`McpRegistry` (`factory.rs`) implements the orchestrator's `RuntimeFactory`: `runtime_for(allowed_mcps,
provenance)` connects to each registered server (via its `McpConnector`), builds a `MultiMcpRuntime`
namespacing every server's tools `<name>:<tool>`, and **scopes** the result to the allowed MCPs.

- **`McpConnector`** (`connector.rs`) — connect to one MCP server, return a boxed `ToolRuntime`
  (transport-erased, so servers of different transports sit side by side in one registry).
  Implementations:
  - `StdioConnector` — runs the server as a **child process**, speaking MCP over its stdin/stdout
    (`turbomcp_transport::ChildProcessTransport`, spawned lazily on first connect). The production
    connector for `McpTransport::Stdio`/`McpTransport::Managed` — and, since MCP-over-stdio doesn't
    care whether the child process is a bare binary or `docker run -i --rm image ...`, also
    `McpTransport::Docker` (`liberado-bootstrap`'s `docker_argv` builds the argv; no dedicated
    connector type needed — see `liberado-bootstrap/src/lib.rs`'s doc comment on `docker_argv` for
    why `--rm` + the child dying is enough cleanup, no explicit `docker stop`).
  - `HttpConnector` — connects to a remote MCP server over streamable HTTP (`McpTransport::Http`).
- **Scoping (`ScopedToolRuntime`)** enforces Decision 4 at runtime: a non-empty `allowed_mcps`
  filters the catalog the model *sees* (by the `<mcp>:<tool>` convention, via `common::mcp_of`) and
  **rejects** an out-of-scope call before it reaches the server. An empty list = no narrowing
  (`ExecuteDirect`); a narrowed list = a subagent's disjoint slice.

Connections are **pooled by default** (M1): `McpRegistry` checks out a healthy runtime per MCP,
rebinds execution `WriteProvenance` on acquire, and returns it on drop (idle TTL from
`tuning.mcp_pooling`). Idle slots are reaped on pool activity **and** by a background tick so
peers that are never re-acquired still release stdio children / HTTP sessions.
`max_in_flight_per_name` (default 4) caps concurrent checkouts/connects per MCP via a semaphore
held for the runtime lifetime — excess acquires wait up to `connect_wait_secs` then fail.
Transport-level invoke failures mark the checkout unhealthy so it is **not** checked back in
(tool `isError` does not kill the pool). Set `tuning.mcp_pooling.enabled = false` for
connect-per-acquisition.

**M1b degraded-catalog routing (landed):** when composition wires
`McpRegistry::with_health_catalog` to the shared `CapabilityCatalog`, connect/transport failures
call `mark_degraded`. Successful **fresh connects** and **successful tool invokes** call
`mark_healthy` (pool checkout alone does not — an idle connection may already be dead).
Degraded entries half-open after a TTL (default 60s) so routing can retry without an accidental
out-of-band acquire. Dispatch / chat / daemon build `DispatchRequest.catalog` from
`CapabilityCatalog::routing_descriptors()` so classifiers never see known-dead peers. Full
`descriptors()` still lists every registered MCP for zone/authority.
**Registration surface:** hand-edited `topology.toml` only (no admin UI, no agent-owned MCPs).
**Hot-reload:** composition holds a cloneable `McpRegistry` + shared `CapabilityCatalog`;
`liberado_bootstrap::apply_mcp_peer_set` / `LiveMcpController::reload_from_config_dir` (and
`POST /api/mcp/reload`) re-apply the desired peer set without process restart — the same
transition boot uses.

## Chat reconnect

The chat tool surface does not take a pool permit. `LiveRegistryRuntime` starts one supervisor
task per registered peer (`McpRegistry::ensure_chat_supervisor`). That task calls
`McpConnector::connect` directly, so a peer can have two sessions: this chat session, and a
dispatch checkout from the pool. The pool path is still one-shot `connect_all_best_effort`.

Each peer task is a small state machine:

1. Connect. The first in-flight attempt is not reported as degraded.
2. On success, publish that peer's tools into the live catalog as `<name>:<tool>`, log
   `MCP reconnected` (`mcp`, `tools`, `attempts`), and mark the health catalog healthy.
3. Wait until the peer is removed, an invoke sees `connection_is_dead`, or the periodic
   `list_tools` probe fails (every 15s). A tool `isError` is not death.
4. On death, drop the tools immediately. A later call fails fast with
   `MCP {name} unavailable, reconnecting`. An unknown name still reports
   `no MCP named '{server}' is in scope for tool '{tool}'`.
5. Wait on capped exponential backoff, then go back to step 1. The unjittered steps are 1s,
   2s, 4s, 8s, 16s, 32s, then 60s. Equal jitter lands in `[step/2, step]`. A zero wait is
   floored to 1ms. Success resets the schedule.

Failures keep the grep string `MCP failed to connect — continuing without it`. The first
failure logs that line. A later failure logs
`MCP failed to connect — continuing without it; still down` only when the unjittered step
changes, including the first wait at the cap. Further waits at the cap are quiet.

Chat boot waits for the first connect wave for at most 15 seconds. The wait ends when
every registered peer has finished its first attempt, or when that bound elapses. A peer
that is still inside `connect`, or that failed, keeps the background retry. Startup does
not wait longer than 15 seconds.

The boot line and every later change use one line, `chat: tool surface ready`, with field
`count`. `count` is the face tool surface: the MCP tools the face lists, plus `delegate`
and `create_agent` in delegation mode, plus the private workspace tools when the workspace
is offered. The supervisor notifies the server when the MCP tool count changes. The server
logs that face count. Boot logs once, after the wait, with `tools` and `delegation_mode`
as well. A later drop or reconnect logs the same face count, so a number from before an
outage and a number from after recovery are the same kind of count.

`GET /api/status` includes `mcp[]` with `name`, `state` (`connected` or `degraded`),
`attempts`, `tools`, and optional `last_error` and `next_retry_ms`. `GET /api/health` stays
`{"ok": true}` and HTTP 200 while a peer is down.

`register` and `replace_connectors` (including `POST /api/mcp/reload`) bump a generation.
A removed peer's task is cancelled, including a backoff sleep. A kept name is restarted so a
URL or command change applies. Adding a peer starts a new task.

## Dependencies

- Depends on: `liberado-executor` (`ToolRuntime`, and `RuntimeFactory`/`RuntimeSetupError` — moved here
  from `liberado-orchestrator` specifically so this crate wouldn't need to depend sideways on the
  dispatch-bridging crate), `liberado-provider` (`ToolDef`/`ToolInvocation`), `liberado-common`
  (`WriteProvenance`, `mcp_of`), `turbomcp-client`, `turbomcp-transport`.
- Depended on by: the daemon/cli wiring (assembles a factory + orchestrator). No cycle —
  `orchestrator` does not depend on `mcp`, and (as of the `RuntimeFactory` move) `mcp` no longer
  depends on `orchestrator` either.

## Tests

- `tests/runtime.rs` — the runtime against a real in-process MCP server (channel transport): catalog
  mapping, invocation, **provenance arriving in `_meta`** (echoed back), unknown-tool→`Err`.
- `tests/factory.rs` — the factory path: full-catalog runtime, `allowed_mcps` scoping, and M1 pooling
  (reuse, disable → no reuse, reconnect after failure, idle TTL expiry) via a connect-count spy.
