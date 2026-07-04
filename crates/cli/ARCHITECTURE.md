# liberado-cli — the `liberado` binary

The single binary: a thin **client + launcher**. It owns nothing but argument dispatch and
tracing-subscriber init; every bit of daemon and chat behavior lives in libraries. The composition
root (wiring the provider/dispatcher/orchestrator into the daemon) now lives in `liberado-server`,
which this binary launches.

## What it does

```
liberado serve <vault>   ──►  liberado_server::run(vault)   (daemon watch loop + chat + HTTP/SSE API)
liberado <vault>         ──►  liberado_server::run(vault)   (back-compat alias for serve)
liberado                 ──►  liberado_server::run($LIBERADO_VAULT)
liberado chat [session]  ──►  chat_client::run(session)     (streaming HTTP/SSE client of a running server)
```

`serve` runs in the foreground, hosting the daemon and API until killed. `chat` is a thin native
(`reqwest`/SSE) client of a separately-running server (`docs/reference/api.md`) — no agent logic, no
provider, no store; it just streams `/api/chat/stream` and prints it.

## Arguments & environment

- **Vault path** from the `serve <vault>` / bare `<vault>` arg, or `LIBERADO_VAULT`.
- `chat` takes an optional `session-id` to resume; `LIBERADO_SERVER` points it at a non-default
  server base URL (default `http://127.0.0.1:4201`).
- Logs to **stderr** (unbuffered, survives shutdown); stdout is left for data.

The daemon's operating mode (watch-only / decide-only / act) is selected by the server from
`DEEPSEEK_API_KEY` (dispatcher) and the enabled `[[mcps]]` in `topology.toml` (execution) — see
`crates/server`.

## Dependencies

- Depends on: `liberado-server` (the launched daemon/API), plus `tokio` + `tracing-subscriber` for
  the runtime and logging, and `reqwest` / `futures` / `serde_json` for the `chat` client.

## Tests

The SSE decoder itself now lives in `chat-client-contract`'s `native::SseDecoder` (shared with the
TUI, extracted from what used to be a `chat_client`-local parser — see
`docs/roadmap/tui-shared-code-extraction-plan.md`), with its own test suite there; `chat_client.rs`
imports it rather than re-implementing it. The rest of this binary's behavior is exercised through
the library crates and verified by live smoke runs (`cargo build --bin liberado`, then `liberado
serve` against a scratch vault with a `chat` client attached).
