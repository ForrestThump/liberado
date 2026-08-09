# ACP Bridge — Completion Roadmap

**Status (2026-08-09):** Core path **shipped**. `liberado-acp` speaks real ACP JSON-RPC 2.0,
runs `Conversation` + `CodingToolRuntime` per session, streams `session/update`, and is
registered in Paseo via generic `extends: "acp"`.

**Install / dogfood:** [`docs/impl/paseo-integration.md`](../impl/paseo-integration.md) ·
`scripts/install-paseo-liberado.ps1` · `config.example/paseo-liberado.json`.

**Paseo side:** no dedicated `liberado-acp-agent.ts` — use the stock Generic ACP provider
(`extends: "acp"`, `command: ["liberado-acp"]`), same as Gemini/Hermes.

---
## What works today

- JSON-RPC 2.0 NDJSON on stdin/stdout (`jsonrpc: "2.0"`, wire methods `session/*`)
- `initialize` → `protocolVersion: 1`, `agentInfo`, `agentCapabilities`
- `session/new` → session id + models/modes; coding tools rooted at `cwd`
- `session/prompt` → `Conversation::turn_stream` + `session/update` (`agent_message_chunk`, tool events)
- `session/cancel` notification → abort in-flight turn → `stopReason: "cancelled"`
- Provider from env (`DEEPSEEK_API_KEY` / OpenRouter / OpenAI) or `LIBERADO_CONFIG_DIR`
- Unit tests for prompt extraction + session payload shape; stdio smoke for initialize/new

## Still open (in priority order)

### 1. Durable session persistence
`session/load` reopens a **fresh** conversation under the same id. Wire
`liberado-session-store` so history survives process restart and Paseo resume.

### 2. Mock-provider integration test
Drive `MockProvider` through the real bridge binary (or a lib-exported loop) and assert
streamed `session/update` events for tool_call + text.

### 3. Tool-call id correlation
Today tool start/finish use independent synthetic ids. Track `toolCallId` across
`ToolStarted` → `ToolFinished` so Paseo UI can pair them.

### 4. Modes (plan / explore)
Map `session/set_mode` onto `PathPolicy::read_only()` / plan-artifact presets already in
`coder-core`.

### 5. Permission mapping (optional)
Only if we want Paseo to mediate Liberado `AskHuman` / write gates — currently Liberado
handles policy inside the agent.

### 6. Remote daemon tunnel (separate item)
Roadmap “Remote access via Paseo” is not this binary: that mates Paseo’s tunnel to
`liberado serve` HTTP/SSE. Keep it distinct from the ACP coding agent.
