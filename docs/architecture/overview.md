# Liberado — Architecture

Liberado is a **Rust-native personal AI Life OS**: a daemon that watches your Obsidian vault, reasons
about changes with an LLM, and acts on your behalf through tools — safely, and without reacting to its
own work. This document is the cold-start map. Each crate has its own zoomed-in
`crates/<name>/ARCHITECTURE.md`.

## Three pillars

For how these pillars position Liberado against the free alternatives, see
[Positioning](positioning.md).

1. **Modular MCP/hook substrate.** Loose-coupled components (perceive / decide / execute / store) that
   talk through events; capability is composed from optional crates. TurboVault is the default,
   privileged perception+storage plugin — not a hard dependency. Every crate aims to be independently
   useful.

2. **Safety is engineered, not prompted.** The LLM proposes; deterministic code disposes, only ever
   toward less autonomy. The capability/zone boundary is the actual trust boundary — and it is what
   makes self-extension safe: an agent can build new tools but can never widen its own authority. The
   default boundaries are MCPs and hooks: those are exactly how agents interact with real data, and
   they are the first and primary point of security and control for the user.

3. **Token efficiency via the dispatcher-as-tool-advisor.** Disjoint context; a compact catalog;
   tools surfaced on-demand, never dumped into context. This applies beyond tool definitions to
   context pollution in general — build everything asking: "are we adding more context than we need,
   too quickly, and can it be surfaced later instead so it stays useful?" The caveat matters: context
   is sometimes unexpectedly useful, so this can be taken too far in the conservative direction.
   Empirical testing finds the balance.

**Operational data** (the runtime trace — Decision 12; conversation history — Decision 17)
deliberately lives *outside* the vault as append-only JSONL, so high-volume writes don't pollute the
change-stream the daemon reacts to; it reaches the vault only as a one-way, derived Markdown export.

## The loop (perceive → decide → act → don't loop)

- AGENT TO DO: CONVERT ALL GRAPHS TO MERMAID BLOCKS ON NEXT DOCS UPDATE RUN.

```
            ┌─────────────────────────── vault (Obsidian Markdown, Turbovault) ───────────────────────────┐
            │                                                                                             │
            ▼                                                                                             │
  ┌──────────────────┐   external    ┌──────────────┐   decision   ┌───────────────┐   tool calls  ┌──────┴───────┐
  │  daemon: watch   │──── change ───▶│  dispatcher  │─────────────▶│ orchestrator  │──────────────▶│   executor  │
  │  debounce        │   (attributed │  classify +  │  Execute /   │ decision →    │   agent loop  │  + ToolRuntime │
  │  attribute ◀─────┼── as External)│   guards     │  Subagent /  │ Task + prov.  │               │  (liberado-   │
  │  (loop-break)    │               └──────────────┘   Clarify    └───────────────┘               │   mcp)       │
  └────────┬─────────┘                                                                              └──────┬───────┘
           │  Agent/Missing → SUPPRESS (our own write)                                                     │
           │                                                                                  writes carry │
           └──────────────── provenance in the audit log ◀──────── _meta provenance ◀─────────────────────┘
```

The dashed return path is the **loop-break** (Decision 5): an agent's tool call carries
`WriteProvenance` in the MCP request `_meta`; Turbovault records it on the write's audit-log entry; the
daemon's `attribute()` then recognizes the resulting vault change as *ours* and suppresses it instead
of reacting. Proven end-to-end in `crates/vault/tests/provenance_e2e.rs`.

## Crate map

Bottom-up (each depends roughly on those above it):

| Layer | Crate | Role |
|---|---|---|
| Types | [`common`](crates/common/ARCHITECTURE.md) | Shared vocabulary: provenance, capability, dispatch, event, model, config, proposal. No logic. |
| Inference | [`provider`](crates/provider/ARCHITECTURE.md) | The `Provider` narrow waist + `MockProvider`. No HTTP. |
| Inference | [`provider-deepseek`](crates/provider-deepseek/ARCHITECTURE.md) | Concrete DeepSeek backend (the only crate that talks to a model). |
| Vault | [`vault`](crates/vault/ARCHITECTURE.md) | Turbovault adapter: provenance writes + hash-join attribution (loop-breaking). |
| Decide | [`dispatcher`](crates/dispatcher/ARCHITECTURE.md) | classify (LLM) → guards (deterministic, downgrade-only) → `DispatchDecision`. |
| Act | [`executor`](crates/executor/ARCHITECTURE.md) | The agent loop: drive a `Provider` over a `ToolRuntime` to a `Report`. MCP-agnostic. |
| Act | [`mcp`](crates/mcp/ARCHITECTURE.md) | `TurbomcpRuntime`: the `ToolRuntime` over real MCP tools; injects provenance into `_meta`. |
| Act | [`orchestrator`](crates/orchestrator/ARCHITECTURE.md) | Bridges a `DispatchDecision` to an execution; chooses the provenance correlation. |
| Converse | [`main-agent`](crates/main-agent/Cargo.toml) | Multi-turn `Conversation`: drives the executor's conversational loop, carries context across turns, streams `AgentEvent`s (tokens, tool start/result), atomic-under-cancel turns. The thing a chat client talks to. |
| Core | [`daemon`](crates/daemon/ARCHITECTURE.md) | The long-running watch→debounce→attribute→dispatch loop. |
| Compose | [`bootstrap`](crates/bootstrap/Cargo.toml) | Builds provider/dispatcher/orchestrator from the environment — the shared composition logic for the `cli` and server binaries. |
| Root | [`cli`](crates/cli/ARCHITECTURE.md) | The single `liberado` binary — client + launcher (`serve` runs the daemon, `chat` streams). |
| Server | [`server`](crates/server/Cargo.toml) | The daemon process — watch loop + chat + HTTP/SSE API (`docs/reference/api.md`); run via `liberado serve`. |
| Web UI | [`webui`](crates/webui/Cargo.toml) | Dioxus WASM frontend — dashboard, reactions feed, vault panel, streaming chat. Excluded from workspace native builds; built with `dx build`. |
| Eval | [`eval`](crates/eval/Cargo.toml) | Real-model routing/safety eval suite (routing accuracy, safe-default rate, UNSAFE-acts that must never increase). Not a build dependency of the system. |

## Cross-cutting concepts

- **Provenance & loop-breaking (Decision 5)** — `WriteProvenance` (`source` + `correlation_id`) rides
  the audit log, not frontmatter. Consumers attribute by content identity (hash-join), not timing.
- **Capability/zone containment (Decision 4)** — `CapabilitySet` is narrow-only; a subagent gets
  `base ∩ narrowing`. This is the actual security boundary.
- **Provider-agnostic inference (Decision 13)** — one `Provider` trait, swappable from config, with
  role-tiered model floors. Tests inject `MockProvider` (Decision 16).
- **MCPs vs hooks** — MCPs are **tools** the agent *calls* (work). Hooks are **event sources** that
  *push* into the daemon (the `Event` type serves both trigger paths). Today only the vault watcher
  produces events.
- **Daemon-first (Decision 2)** — one long-running process; the CLI/TUI attach to it.

## Co-development with Turbovault & Turbomcp

`turbovault/` and `turbomcp/` are **sibling repos**, consumed as path dependencies during
co-development (Decision 7) and excluded from this workspace. A root `[patch.crates-io]` redirects
Turbovault's published `turbomcp` to the local fork so the whole tree builds against one Turbomcp —
the one carrying the request-`_meta` pass-through that the provenance loop depends on. (Those upstream
changes live on feature branches and have a draft issue in `turbomcp-request-meta-issue-draft.md`.)

## Current status

The reactive backbone, the web UI, a **streaming conversational chat loop**, and **persisted,
session-keyed conversations** (Decision 17) are complete, all hosted by **one `liberado` binary**
(daemon-first, Decision 2 — `serve` hosts everything; `chat` is a client). The next work is deepening
the main agent (context policy + dispatcher integration) and the TUI.

**Done:**
1. ✅ **Reactive pipeline** — daemon watches → attributes → dispatches → orchestrates → executes, end-to-end wired and tested.
2. ✅ **Concrete `RuntimeFactory`** — `liberado-mcp`'s `TurbomcpRuntimeFactory` connects via stdio, builds a provenance-bound `TurbomcpRuntime`, scopes it to allowed MCPs.
3. ✅ **Daemon → orchestrator** — `react()` runs dispatch → orchestrate; `Reaction` carries `ReactionOutcome` (`Observed` / `Decided` / `Acted(Disposition)`). The server assembles the orchestrator from the enabled `[[mcps]]` in `topology.toml`, each connected by `transport` (`crates/bootstrap`'s `mcp_registry_from_config`).
4. ✅ **Single-binary consolidation** — one `liberado` binary with subcommands: `liberado serve [vault]` (daemon + chat + HTTP/SSE API), `liberado chat [session]` (client), bare `liberado <vault>` aliases `serve`. `crates/server` (`liberado-server`) is a **library** exposing `pub async fn run(vault)`, not a binary. This concretely realizes daemon-first (Decision 2): one process hosts everything; every interface is a client.
5. ✅ **Web UI** — `liberado-server` (Axum, `:4201`, run via `liberado serve`) hosts the daemon and serves a JSON API; `liberado-webui` (Dioxus WASM) is the browser dashboard showing daemon status, reactions, and vault info. LAN-accessible. Build with `dx build --release --package liberado-webui --web` (see `AGENTS.md`).
6. ✅ **Conversational chat loop** — `crates/main-agent`'s `Conversation` drives the executor's conversational tool-calling loop with context carried across turns. Served over the shared chat/SSE contract (`docs/reference/api.md`): `POST /api/chat` and the streaming `GET`/`POST /api/chat/stream` with token streaming, tool-call visibility (`tool`/`tool_result`), and stop/cancel (close the stream → turn aborts + history rolls back, persisting nothing).
7. ✅ **Conversation persistence** — Decision 17 landed: `crates/conversation-store` (`liberado-conversation-store`) is the append-only JSONL log of DAG message-nodes (ULIDs minted at append time inside a per-conversation lock, so file-order == id-order), outside the vault under `<LIBERADO_DATA_DIR>/conversations`. `main-agent`'s `ChatSessions` rehydrates per turn from the store and **persists only on success**, so the server holds no in-memory conversation cache and a cancelled turn is a clean on-disk no-op. Sessions are keyed by a `session` SSE event; `GET /api/conversations` + `GET /api/conversations/{id}` list and reopen them.
8. ✅ **`liberado chat` CLI client** — a `reqwest`/SSE terminal REPL (`crates/cli/chat_client.rs`), the first native (non-browser) client of the shared chat API and the seed of the future TUI.
9. ✅ **Config-driven substrate** — the daemon boots on one validated `Config` (Decision 14, `crates/bootstrap`): the dispatcher holds `policy.toml`'s grants as its base authority, and `topology.mcps` is now the **single source** for both the dispatcher's catalog AND the runtime's MCP connection. Each `[[mcps]]` entry declares a required `description` (routing), `consequence` (the risk gate), and `transport` (`stdio` command/args or `http` url — how the runtime reaches it); the dispatcher routes over the enabled MCPs and the orchestrator connects to those same names by transport, so a routed name is always a name the runtime can reach (slice 2b done — no env path remains).

10. ✅ **Proposal workflow (Decision 11, emit AND approve→execute)** — the full propose→approve→execute loop is closed. The EMIT path writes a `proposals/<id>.md` artifact for high-consequence concrete actions (YAML frontmatter with `status: pending`); the APPROVE→EXECUTE half picks up a human `status: approved` edit via the watch loop, calls `orchestrator.execute_approved()` with the proposal's `correlation_id` as provenance (no re-dispatch, no guards — the edit is the authorization), and flips `status` to `done` (loop-broken, idempotent).

**Not yet built (next slice):**
- **Main-agent depth** — the conversational loop exists, but the fuller design (ContextPolicy header, dispatcher integration so chat routes through the same guards as reactions, per-turn background surfacing) is not wired yet; chat currently drives the executor directly.
- `crates/tui/` — ratatui TUI client that attaches to the daemon over the same chat/SSE contract (`liberado chat` already proves the contract is client-agnostic).
- Inbox hook, hooks generally, multi-MCP registry, connection pooling.

## Where to start reading

1. This file.
2. [`common`](crates/common/ARCHITECTURE.md) — the vocabulary everything speaks.
3. [`vault`](crates/vault/ARCHITECTURE.md) — attribution / loop-breaking (the conceptual heart).
4. [`dispatcher`](crates/dispatcher/ARCHITECTURE.md) → [`orchestrator`](crates/orchestrator/ARCHITECTURE.md)
   → [`executor`](crates/executor/ARCHITECTURE.md) — the decide→act path.
5. [`daemon`](crates/daemon/ARCHITECTURE.md) — how it all runs.
6. [`../contributing/development-workflow.md`](../contributing/development-workflow.md) — before
   starting non-trivial work: the research/plan/implement/test/document/commit process this project is
   held to, and how to delegate to subagents effectively.

The deeper "why" behind each Decision N lives in the root planning docs (`*-spec.md`,
`life-os-architecture.md`).
