# Liberado

Rust-native **personal AI Life OS** and **agentic orchestration** substrate: one daemon watches a vault, reasons with an LLM, and acts through MCP tools under capability/zone containment — without reacting to its own writes (provenance loop-break). Surfaces (TUI, WebUI, CLI, Telegram) are clients; they do not own the loop.

## Documentation hub

**All product docs live under [`docs/`](docs/README.md).** Start there.

| For… | Go to… |
|------|--------|
| First run | [docs/getting-started/quickstart.md](docs/getting-started/quickstart.md) |
| How it works | [docs/architecture/overview.md](docs/architecture/overview.md) |
| Sessions model | [docs/architecture/sessions.md](docs/architecture/sessions.md) |
| Frozen seams | [docs/architecture/contracts.md](docs/architecture/contracts.md) |
| What to build next | [docs/roadmap/current.md](docs/roadmap/current.md) |
| Live homelab status | [docs/handoff.md](docs/handoff.md) |
| HTTP/SSE API | [docs/reference/api.md](docs/reference/api.md) |
| Crate inventory | [docs/reference/crate-map.md](docs/reference/crate-map.md) |
| Failure-modes checklist | [docs/architecture/failure-modes.md](docs/architecture/failure-modes.md) |
| Open design questions | [docs/design_questions_for_the_user.md](docs/design_questions_for_the_user.md) |

## Strategy (short)

**Daemon (life-ops) first → chat surface → coding pack.** Sequencing and competitive framing: [docs/architecture/positioning.md](docs/architecture/positioning.md).

## Development

- Workspace: Cargo crates under [`crates/`](crates/)
- Agent build/run notes: [docs/contributing/agents.md](docs/contributing/agents.md)
- Layer rules (mechanical): `crates/test-support/tests/layer_rules.rs`
- Example config: [`config.example/`](config.example/)

Nested MCP checkouts (`liberado-*-mcp/`, `turbovault/`, …) may appear for co-dev; they are **not** the Liberado workspace product docs.

## License

See [LICENSE](LICENSE).

**Last updated:** 2026-07-23 — docs hub reorg; architecture hardening (module splits, T1 partial, MCP pooling) on branch.
