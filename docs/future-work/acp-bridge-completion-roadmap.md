# ACP Bridge — Completion Roadmap

**Status (2026-08-09):** **Superseded as the living backlog** by
[`paseo-liberado-integration-roadmap.md`](paseo-liberado-integration-roadmap.md).

Core path remains **shipped**: `liberado-acp` speaks real ACP JSON-RPC 2.0, runs
`Conversation` + `CodingToolRuntime` per session, streams `session/update`, and is registered
in Paseo via generic `extends: "acp"`.

**Use the integration roadmap** for ordered work (P0 tool-call ids, resume honesty, `--version`,
tests, modes, durable load, fork polish, remote track). This file is kept so older links still
resolve; do not extend the residual list here.

**Install / dogfood:** [`docs/impl/paseo-integration.md`](../impl/paseo-integration.md) ·
`scripts/install-paseo-liberado.ps1` · `config.example/paseo-liberado.json`.

**Paseo side:** no dedicated `liberado-acp-agent.ts` required — stock Generic ACP provider
(`extends: "acp"`, `command: ["liberado-acp"]`), same as Gemini/Hermes. Optional first-class
provider is Phase 5 on the integration roadmap.
