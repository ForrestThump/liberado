# Liberado — Handoff

Current-state handoff for the next session. For the full system map read `ARCHITECTURE.md`; for
build/run read `AGENTS.md`; for the chat API read `docs/reference/api.md`; for the rationale behind any
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
- Chat/SSE contract: `docs/reference/api.md`. Endpoint table + env vars: `AGENTS.md`.

## Matured vision (2026-06-26 planning session)

Liberado is a **modular MCP/hook-first Rust agent substrate**, **vault-optional**, whose
differentiation is **"self-improving autonomy with guarantees"**: the LLM proposes, deterministic
code disposes, and self-extension can build new tools but can never widen its own authority. Full
thesis + competitive grounding in [`docs/architecture/positioning.md`](../architecture/positioning.md);
the three pillars in [`docs/architecture/overview.md`](../architecture/overview.md); the seam plan in
[`docs/architecture/modularity.md`](../architecture/modularity.md).

**The proposal loop (Decision 11) is fully landed** — both EMIT and APPROVE→EXECUTE. A
high-consequence concrete action emits a `proposals/<id>.md` artifact; a human `status: approved`
edit is picked up by the watch loop and executed via `orchestrator.execute_approved()` with the
proposal's `correlation_id` (no re-dispatch, no guards — the edit is the authorization), then flipped
to `done` (loop-broken, idempotent).

**Four planning decisions agreed this session:**

1. **General-agent-first** — the next milestone is the vault-agnostic general MCP agent, built
   mesh-native and crate-independent (none of it touches the vault).
2. **Incremental mesh with checkpoints** (Decision 18) — wrap seams behind an `EventBus` trait as
   they are touched; new components are bus-native from day one; concrete "mesh is real now"
   checkpoints tied to features guard against drift. Not a big-bang refactor.
3. **Vault = hard-plugin destination, reached via the mesh** (Decision 19) — TurboVault is the
   privileged *default* perception+storage plugin in the meantime, but the core is vault-agnostic;
   the vault becomes a plugin behind an event-source/hook trait in Phase 3.
4. **Personal-first with framework-grade seams** — build what is objectively more useful for the
   author than the free alternatives, but keep crate boundaries clean enough to reuse.

## Known issues

- **Known bug (Phase 1 blocker) — MCP tool names with `:` rejected by the provider API.** Symptom:
  chat/TUI with an MCP configured returns HTTP 400 `Invalid 'tools[0].function.name': ... pattern
  '^[a-zA-Z0-9_-]+$'`; the agent appears to see no tools. Root cause: Liberado namespaces MCP tools
  as `<mcp>:<tool>` (the `mcp_of` convention), but the OpenAI/DeepSeek chat API requires
  `tools[].function.name` to match `^[a-zA-Z0-9_-]+$` (no colon), so any tool-bearing request is
  rejected before the model sees the tools. Fix: in the `provider-deepseek` adapter (the
  OpenAI-compatible boundary), sanitize each tool name to the valid pattern on the way out while
  keeping a per-request `sanitized -> original` map, and translate the model's returned tool-call name
  back on the way in. Preserves the internal `:` convention; handles any invalid char
  (colon/dot/slash). This is the next debugging task.

## What's next — Phase 1: the general MCP agent

(See [`docs/roadmap/current.md`](../roadmap/current.md) for the full four-phase plan.)

The immediate Phase-1 work is (a) fix the MCP tool-name bug above, then (b) route chat through the
dispatcher. Riggers integration is now roadmapped (use-as-MCP + Provider-trait adoption) as the
Phase-2 self-improvement engine — see the roadmap.

1. **Fix the MCP tool-name `:` bug** (see Known issues) — sanitize tool names at the
   `provider-deepseek` boundary with a per-request reverse map. Unblocks every tool-bearing request.
2. **Route chat through the dispatcher** — chat currently drives the executor directly, bypassing the
   tool-advisor, guards, and sub-delegation; wiring chat -> dispatcher -> orchestrator gets all three
   (and is the first bus-native seam: chat publishes a goal-event).
3. **Live capability catalog + on-demand tool surfacing** — a live, bus-queryable registry (mesh
   checkpoint #1), the token-efficiency core.
4. **Multi-MCP + parallel, capability-narrowed sub-delegation** (closes Hermes gap #4).
5. **`crates/tui`** — ratatui client over the same chat/SSE contract; the near-term modularity proof
   is extracting a `chat-client-contract` crate the TUI depends on alone.
6. Roadmap items still open: runtime tool gating, MCP connection pooling, multi-server registry UX.

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
