# liberado-cli — the `liberado` binary

The composition root: the one place concrete implementations are wired into the daemon and the watch
loop is run. Thin by design — all behavior lives in the libraries; this just assembles them.

## What it wires

```
DeepSeekProvider::from_env()  ──►  Arc<dyn Provider>  (shared)
        │                                    │
        ▼                                    ▼
Dispatcher::new(provider, …)         Orchestrator::new(provider,
        │                              TurbomcpRuntimeFactory::new(
        │                                StdioConnector::new(cmd, args)))
        ▼                                    │
Daemon::open(vault)                          │
   .with_dispatcher(dispatcher, …)           │
   .with_orchestrator(orchestrator) ◀────────┘   (only when LIBERADO_MCP_CMD set)
        │
        ▼
run()  ──► reactions logged to stderr
```

The one `Arc<dyn Provider>` is shared by the dispatcher (classification) and the orchestrator's
executor (the agent loop).

## Modes (by what's configured)

| Configured | Mode | Reaction outcome |
|---|---|---|
| no `DEEPSEEK_API_KEY` | watch-only | `Observed` |
| key, no `LIBERADO_MCP_CMD` | decide-only | `Decided` |
| key + `LIBERADO_MCP_CMD` (+ optional `LIBERADO_MCP_ARGS`) | **act** | `Acted` |

- **Vault path** from the `<vault-path>` arg or `LIBERADO_VAULT`.
- `LIBERADO_MCP_CMD` is the MCP server command `StdioConnector` spawns; `LIBERADO_MCP_ARGS` its
  whitespace-separated args.
- Logs to **stderr** (unbuffered, survives shutdown); stdout is left for data.

## Dependencies

- Depends on: `liberado-daemon`, `liberado-dispatcher`, `liberado-orchestrator`, `liberado-mcp`,
  `liberado-provider`, `liberado-provider-deepseek`, plus `tokio` + `tracing-subscriber`.

## Tests

Behavior is exercised through the library crates; the binary itself is verified by live smoke runs
(`cargo build --bin liberado` then run against a scratch vault).
