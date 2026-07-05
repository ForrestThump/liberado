# Modularity — The Seam Plan

This page records the concrete moves that turn the three pillars' "modular MCP/hook substrate" into
reality. It is the engineering companion to the [mesh vision](../ideas/meshify.md) and the
[roadmap's mesh checkpoints](../roadmap/current.md).

## The test that keeps loose coupling honest

**"Could someone use just this crate?"** — applied per-crate. If a crate can only be used as part of
the whole, the boundary is not real yet. `conversation-store` already passes: it is standalone (the
`ConversationStore` trait + `JsonlStore`, no dependency on the daemon, the dispatcher, or the vault).

## Concrete seam moves

- **Extract a `chat-client-contract` crate** — done. Holds the shared wire DTOs and the
  `SseDecoder` incremental parser. **TUI depends only on that** for its wire/transport-framing
  needs, making it a standalone TUI-for-any-agent library rather than a Liberado-coupled binary.
  A `ChatClient` trait was tried here too (one shared `send`/`stream` implementation for every
  client) but deleted 2026-07-05 — TUI's non-blocking render loop and the CLI's blocking REPL
  turned out to need different enough transport shapes that forcing one trait wasn't worth it;
  `SseDecoder`/`ChatEvent::from_sse_data` are the real, working seam.
- **Define an event-source / hook trait** — the seam that makes the vault a plugin. Vault-watch and
  cron both implement it, so the daemon consumes events without knowing where they came from. This is
  what demotes TurboVault from hard dependency to default-privileged plugin (see Decision 19).
- **Dispatcher-as-library** — the tool-advisor is independently useful on its own (route a goal over
  a catalog, with downgrade-only guards). Keep its dependency surface clean so it can be consumed
  outside the daemon.
- **Introduce the `EventBus` trait as seams are touched** — not as a big-bang refactor. Phase 1's
  chat -> dispatcher wiring is the first one. New components are bus-native from day one; old ones
  migrate when touched. (Decision 18.)

## Where this connects

- The [mesh vision](../ideas/meshify.md) is the destination these seams lead to.
- The [roadmap](../roadmap/current.md) ties each seam to a feature phase and a "the mesh is real now"
  checkpoint, so modularity is verified by shipped behavior rather than asserted.
