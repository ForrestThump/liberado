# Liberado

Rust-native **personal AI Liberado** and **agentic orchestration** substrate: one daemon watches a vault, reasons with an LLM, and acts through MCP tools under capability/zone containment — without reacting to its own writes (provenance loop-break). Surfaces (TUI, WebUI, CLI, Telegram) are clients; they do not own the loop.

## Documentation hub

**All product docs live under [`docs/`](docs/README.md).** Start there.

| For… | Go to… |
|------|--------|
| First run | [docs/impl/getting-started/quickstart.md](docs/impl/getting-started/quickstart.md) |
| How it works | [docs/spec/architecture/overview.md](docs/spec/architecture/overview.md) |
| Sessions model | [docs/spec/architecture/sessions.md](docs/spec/architecture/sessions.md) |
| Frozen seams | [docs/spec/architecture/contracts.md](docs/spec/architecture/contracts.md) |
| Product direction | [docs/direction.md](docs/direction.md) |
| What to build next | [docs/future-work/backlog.md](docs/future-work/backlog.md) |
| HTTP/SSE API | [docs/spec/reference/api.md](docs/spec/reference/api.md) |
| Crate inventory | [docs/spec/reference/crate-map.md](docs/spec/reference/crate-map.md) |
| Failure-modes checklist | [docs/spec/architecture/failure-modes.md](docs/spec/architecture/failure-modes.md) |

## Strategy (short)

Personal assistant first. Liberado owns the data, the rules, and reliable delivery: tasks, reminders, budget, and recovery. The coding pack is frozen. Next build work is the security and reliability basics in the [backlog](docs/future-work/backlog.md): close the control plane, then make delivery durable.

Direction: [docs/direction.md](docs/direction.md). The July 2026 competitive note is historical: [docs/spec/architecture/positioning.md](docs/spec/architecture/positioning.md).

## Development

- Day-to-day build: `just build-slim` (full workspace: `just build`)
- Workspace: Cargo crates under [`crates/`](crates/)
- Contributor and agent orientation: [AGENTS.md](AGENTS.md)
- Layer rules (mechanical): `crates/test-support/tests/layer_rules.rs`
- Example config: [`config.example/`](config.example/)
- Agent playbooks + coder prompts: [`skills/`](skills/) (coder prompts under `skills/coder/`)
- Quality-metric baselines: [`code-metrics/`](code-metrics/)

Nested MCP checkouts (`liberado-*-mcp/`, `turbovault/`, …) may appear for co-dev; they are **not** the Liberado workspace product docs.

## License

See [LICENSE](LICENSE).

**Last updated:** 2026-10-25 — strategy follows [docs/direction.md](docs/direction.md). The work queue is the backlog.
