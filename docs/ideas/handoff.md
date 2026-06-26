# Liberado — Handoff

Current-state handoff for the next session. For the full system map read `ARCHITECTURE.md`; for
build/run read `AGENTS.md`; for the chat API read `docs/interface.md`; for the rationale behind any
"Decision N" read `liberado-architecture-decisions.md`.

> Note: this is the single handoff file. There is **no** uppercase `HANDOFF.md` — on Windows
> (case-insensitive filesystem) the two names collide, so keep it lowercase. (An earlier stub here
> pointed at a nonexistent uppercase file; that was the bug, now fixed.)

## State of the tree

All current work is **uncommitted** (one commit in history: `first commit`). `cargo test --workspace`
is green (all crates pass). The system is daemon-first and consolidated into **one binary**.

## What's done (this arc)

- **Decision 17 — conversation persistence, end to end.** New crate `crates/conversation-store`
  (`liberado-conversation-store`): an append-only JSONL log of DAG message-nodes, one file per
  conversation under `<LIBERADO_DATA_DIR>/conversations` (outside the vault — Decision 12 operational
  data). `ConversationStore` trait + `JsonlStore` impl; spec in `liberado-conversation-store-spec.md`.
  The load-bearing property: **ULIDs are minted at append time inside a per-conversation lock**, so
  file-order == id-order — the one choice that can't be retrofitted (enables O(log n) id lookup
  later). Reads are lock-free.
- **`ChatSessions`** (`crates/main-agent/src/sessions.rs`) wires the store into chat:
  **rehydrate-per-turn** from the store, **persist-only-on-success**. The server holds no in-memory
  conversation cache; a cancelled/errored turn writes nothing (composes with the in-memory rollback in
  `Conversation::turn_stream`). The system prompt is persisted as the conversation's root node.
- **Server is session-keyed.** A `session` SSE event leads each stream (client echoes it back as
  `?session=` / the `session` body field); added `GET /api/conversations` and
  `GET /api/conversations/{id}`.
- **Single-binary consolidation.** `crates/webui-server` was renamed to `crates/server`
  (`liberado-server`) and is now a **library** (`pub async fn run(vault)`), not a binary. There is one
  binary, `liberado`, with subcommands: `liberado serve [vault]` (daemon + chat + API),
  `liberado chat [session]` (client), bare `liberado <vault>` aliases `serve`.
- **`liberado chat` CLI client** (`crates/cli/src/chat_client.rs`) — a `reqwest`/SSE terminal REPL,
  the first native client of the shared chat API and the seed of the future TUI.
- **`crates/bootstrap`** (`liberado-bootstrap`) — deduped the env→daemon assembly
  (`provider_from_env`, `mcp_registry_from_env`, `configure_daemon`) so the binary and any other host
  wire the daemon identically.

## Where key things live

- Conversation store: `crates/conversation-store/src/{jsonl,store,types,error}.rs`.
- Chat persistence orchestration: `crates/main-agent/src/sessions.rs` (`ChatSessions`).
- The binary entry / arg dispatch: `crates/cli/src/main.rs`; chat client: `crates/cli/src/chat_client.rs`.
- Server (library): `crates/server/src/{lib,api,state}.rs` — `lib.rs::run` is the daemon entry point.
- Shared env wiring: `crates/bootstrap/src/lib.rs`.
- Chat/SSE contract: `docs/interface.md`. Endpoint table + env vars: `AGENTS.md`.

## What's next

1. **Main-agent depth** — wire the fuller design: ContextPolicy header, dispatcher integration so chat
   routes through the same guards as reactions, per-turn background surfacing. Chat currently drives
   the executor directly.
2. **`crates/tui`** — ratatui client over the same chat/SSE contract. `liberado chat` already proves
   the contract is client-agnostic; the TUI is the next client.
3. **Web UI polish** — Markdown rendering of agent answers; a Stop button wired to
   `EventSource.close()` (the backend cancel primitive already exists).
4. Roadmap items: runtime tool gating, catalog population, zone write-class guard, MCP connection
   pooling, multi-server registry UX (see `ROADMAP.md`).

## Working patterns that succeed (keep doing)

- **Dispatch a subagent for code, then verify independently**: read the produced code, run
  `cargo test`, and run a **live end-to-end smoke** — don't trust the subagent's report alone.
- **Live smoke recipe** (proven repeatedly): hydrate the key from the Windows User env via
  `[Environment]::GetEnvironmentVariable("DEEPSEEK_API_KEY","User")` (NEVER print it; only confirm
  length 35 / prefix `sk-`), start `liberado serve <scratch-vault>` on a scratch `LIBERADO_PORT` +
  `LIBERADO_DATA_DIR`, drive a two-turn message through `liberado chat`, then assert continuity (turn 2
  recalls a word from turn 1) and a persisted ULID `.jsonl` under `<LIBERADO_DATA_DIR>/conversations`.
- A force-killed server (`Stop-Process -Force`) exits **255** — expected, not a failure.

## Pitfalls learned (don't relearn)

- **Windows is case-insensitive**: `handoff.md` and `HANDOFF.md` collide. Keep one lowercase handoff.
- **SSE event naming**: the error event is named **`failed`**, not `error` — browser `EventSource`
  reserves `error` for its own connection errors. Structured events (`tool`, `tool_result`) are
  JSON-encoded so multi-line previews don't split across `data:` lines.
- **`tokio::select!` borrow tangles**: don't reuse the same `&mut` in one branch's body that another
  branch's future borrowed; clone the channel sender, and keep rollback **inside** the awaited future
  (a Drop guard) rather than in the select arm.
- **Keep the `liberado-orchestrator` dep wherever `RuntimeFactory::runtime_for` is called** — the
  trait must be in scope. A removal that looked safe broke the build.
- The executor's **"termination follows the consumer"** design is the seam that makes
  chat-vs-autonomous a configuration, not a fork.

## Live constraints (must not violate)

- **Never print or echo `DEEPSEEK_API_KEY`** (or any secret) — confirm only length/prefix.
- `turbovault` / `turbomcp` PR branches push to the **`ForrestThump` fork only** — no upstream PRs
  without explicit permission.
- **Outward-facing actions need confirmation.**
- Don't commit, push, or run servers/daemons without being asked.
