# Modularity — The Seam Plan

This page records the concrete moves that turn the three pillars' "modular MCP/hook substrate" into
reality. It is the engineering companion to [contracts.md](contracts.md) (the narrow-waist
inventory) and the [roadmap's modularity checkpoints](../../roadmap.md). (The original
[meshify idea](../../future-work/ideas/archive/meshify.md) is annotated — partly landed via traits + config, partly
rejected; the canonical vocabulary is now kernel · domain packs · stores · surfaces.)

## The test that keeps loose coupling honest

**"Could someone use just this crate?"** — applied per-crate. If a crate can only be used as part of
the whole, the boundary is not real yet. `conversation-store` already passes: it is standalone (the
`ConversationStore` trait + `JsonlStore`, no dependency on the daemon, the dispatcher, or the vault).

**Agentic extension:** **"Could a second domain pack plug in without forking the kernel?"** — if a
change only makes sense for git/diff/coding, it belongs in the coding pack, not in shared
orchestration types.

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

## Agentic orchestration seams (kernel vs domain packs)

See [agentic-loops.md](agentic-loops.md) and the
[hygiene audit](../../future-work/archive/agentic-mesh-hygiene-audit-2026-07-10.md).

| Seam | Reusable kernel | Coding domain pack (first) |
|---|---|---|
| Inner tool loop | `liberado-executor` + `ToolRuntime` | — |
| Domain tools | trait only | `coder-tools` + `coder-sandbox` |
| Goal session composition | *logical* Goal / terminals / attempts / role graph | `coder-agent` (first implementation) |
| Contracts | `Report`, `Outcome`, `CapabilitySet`, `Provider` | `coder-core` specializes; converts at boundary |
| Surfaces | session/event API (target) | PR factory process adapter; later TUI/WebUI |

**Rules:**

1. **`ToolRuntime` is the domain limb** — coding tools and MCP tools are interchangeable from the
   executor's point of view. A non-coding goal is "different runtime + different verifiers," not a
   second agent engine.
2. **Surfaces consume contracts** — PR factory, future TUI, and evals do not embed loop control flow.
3. **Verifiers are code, not prompts** — domain packs plug checks; the model never owns the gate.
4. **Design neutral seams now; extract crates when friction is real** — do not wait for a second
   domain to *think* about `Goal` / session events / terminals (those are architecture). Do wait to
   *promote* types into `liberado-common` or a `session` crate until a second pack would otherwise
   copy `coder-core` or depend on coding crates for non-coding work.
5. **Coding pack must not become the product center** — crate names say `coder-*` because they are a
   pack; docs and dependency rules must keep the kernel general.

### Extraction trigger (when to lift types out of `coder-core`)

Extract domain-neutral session vocabulary when **any** of:

- a non-coding goal session needs Goal/attempt/terminal/event types; or
- TUI/WebUI need one event stream for chat + coding + life-ops; or
- a second pack would take a dependency on `liberado-coder-core` for non-coding reasons.

Until then, coding types stay specialized and map to kernel `Report`/`Outcome` at boundaries.

> **Status 2026-07-11 (audit): the trigger fired and was fixed the same day — by inversion, not
> extraction.** `liberado-config-loader` had grown a *real* dependency on `liberado-coder-core`
> (the whole `[coder]` tuning vocabulary: `CoderTuning`, role configs, `VerifierSpec`, …), putting
> the coding pack underneath the entire config stack. Since nothing consumed the typed
> `Tuning::coder` yet, the fix was the cheaper, more general one: `Tuning::coder` is now an
> **opaque `toml::Value`** in config-loader, and `liberado_coder_core::CoderTuning::from_value`
> parses + validates it at composition time — the pack owns its own config section (design rule
> "domain packs load their own role/policy sections"), and fail-fast at boot is preserved in the
> pack's parser. A second pack's section generalizes `Tuning` to a name → raw-value map.
> Verify-DTO extraction into a `liberado-verify` crate remains available later, but is no longer
> forced by config layering. See
> [architecture-alignment-audit-2026-07-11](../../future-work/archive/architecture-alignment-audit-2026-07-11.md).

## Where this connects

- [contracts.md](contracts.md) is the inventory of the frozen seams these moves protect; the
  generated [crate map](../../spec/reference/crate-map.md) shows where every crate sits.
- The [roadmap](../../roadmap.md) ties each seam to a feature phase and a "the seam is real
  now" checkpoint, so modularity is verified by shipped behavior rather than asserted.
- [Agentic loops](agentic-loops.md) describe kernel vs domain packs and the second-domain reusability
  test.
