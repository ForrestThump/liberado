# liberado-mcp — the MCP integration layer (executor ⇄ real MCP tools)

Implements the executor's `ToolRuntime` over `turbomcp-client`, so the agent loop can drive a real
MCP server's tools, and provides the **factory** that the orchestrator uses to obtain those runtimes
on demand. This is the crate that makes tool calls *actually happen* — and the crate that makes
tool-mediated vault writes **self-attributed** (loop-broken).

Three pieces:
- `TurbomcpRuntime` — the `ToolRuntime` itself (below).
- `ClientConnector` + `StdioConnector` — how to obtain a connected client (`connector.rs`).
- `TurbomcpRuntimeFactory` — the `RuntimeFactory` the orchestrator depends on (`factory.rs`).

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

## The factory + connectors

`TurbomcpRuntimeFactory<C: ClientConnector>` implements the orchestrator's `RuntimeFactory`:
`runtime_for(allowed_mcps, provenance)` connects (via the injected `ClientConnector`), builds a
`TurbomcpRuntime` bound to that provenance, and **scopes** it to the allowed MCPs.

- **`ClientConnector`** (associated `Transport`) abstracts connection so the same factory serves the
  production path and tests. `StdioConnector` is the production connector — it runs an MCP server as
  a **child process** and speaks MCP over its stdin/stdout (the process is spawned lazily by
  `initialize()`'s transport auto-connect). Tests inject an in-process channel connector.
- **Scoping (`ScopedToolRuntime`)** enforces Decision 4 at runtime: a non-empty `allowed_mcps`
  filters the catalog the model *sees* (by the `<mcp>:<tool>` convention, via `common::mcp_of`) and
  **rejects** an out-of-scope call before it reaches the server. An empty list = no narrowing
  (`ExecuteDirect`); a narrowed list = a subagent's disjoint slice.

v1 opens a fresh connection per execution (no pooling) and assumes one server per connector;
connection reuse and a multi-server registry are later refinements.

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
- `tests/factory.rs` — the factory path: full-catalog runtime, and `allowed_mcps` scoping (catalog
  filtered + out-of-scope call blocked).
