# Phase 1 — The General MCP Agent: Implementation Plan

## Overview

Phase 1 transforms Liberado from a vault-first reactive daemon into a **vault-agnostic general MCP
agent**. The chat experience gains the dispatcher's tool-advisor, safety guards, and sub-delegation;
multiple MCP servers connect simultaneously; the capability catalog becomes a live, queryable
registry; and a standalone TUI client proves the system is client-agnostic.

This phase delivers the first two of the [three architectural pillars](../../architecture/overview.md):

1. **Insert the dispatcher into the streaming chat loop** — the dispatcher's value (tool-advisor,
   runtime safety guards, sub-delegation) is applied *within* the existing streaming conversational
   loop, so chat gains the same deterministic guard pipeline that gates reactive daemon actions
   without regressing token streaming or multi-turn context. This is also the first bus-native seam
   (Decision 18: chat publishes a goal-event).
2. **Live capability catalog + on-demand tool surfacing** — the token-efficiency core. (Mesh
   checkpoint #1.)
3. **Multi-MCP + parallel, capability-narrowed sub-delegation** — closes [Hermes gap
   #4](../../ideas/vs-hermes.md).

Vault coupling is explicitly excluded: none of the Phase 1 work touches `liberado-vault`, the
daemon's watch loop, or the proposal workflow — proving "the core runs without TurboVault" is a
deliberate property of this milestone.

---

## Current State (Starting Point)

### What's built

| Component | Crate | Status |
|-----------|-------|--------|
| Dispatcher (classify + guards) | `liberado-dispatcher` | Implemented + tested; **only wired into the daemon's watch loop** |
| Orchestrator (decision → execution) | `liberado-orchestrator` | Implemented + tested; same daemon-only wiring |
| Executor (agent loop) | `liberado-executor` | Implemented + tested; both `execute()` and `converse()` modes |
| MCP runtime (turbomcp-backed) | `liberado-mcp` | Implemented + tested; single-server, fresh connection per execution |
| Main agent (Conversation + ChatSessions) | `liberado-main-agent` | Implemented + tested; **drives executor directly, bypassing dispatcher** |
| Daemon (watch → attribute → dispatch → orchestrate) | `liberado-daemon` | End-to-end wired; tested with mock dispatchers/orchestrators |
| Conversation persistence (JSONL) | `liberado-conversation-store` | Landed; Decision 17 complete |
| Proposal workflow (emit + approve→execute) | config/policy/topology | Landed; Decision 11 complete |
| Bootstrap (config → daemon wiring) | `liberado-bootstrap` | Implemented; produces catalog + capabilities from config |
| TUI | `liberado-tui` | Crate scaffolded; no dispatcher/multi-MCP integration yet |

### The gap

**Chat bypasses the dispatcher entirely.** In `crates/server/src/lib.rs:build_chat()`, the server
builds a `ChatSessions` with an `Executor` and a `ToolRuntime` connected directly to the MCP server.
The user's message goes straight to the model via `executor.converse_messages()`. There is no
tool-advisor scoping, no guard evaluation, no sub-delegation, and no capability enforcement — the
chat path is missing the safety properties the daemon's reactive path enjoys.

Chat's safety must come from **capability scoping** (which relevant tools the runtime is allowed to
surface) *plus* a **runtime guard** that applies the consequence/magnitude gate to each adaptive
tool call — that combination is what makes chat safe. The daemon's *pre-flight* dispatcher guards
alone do not cover chat: they only see a goal/seed, whereas chat tool calls are decided by the model
mid-conversation, so the gate has to live at the runtime boundary.

```
Current chat flow:
  user message → Executor.converse_messages() → model + tools → streamed reply

Current daemon flow (pre-flight gate):
  vault change → Dispatcher.classify() → Guards.evaluate() → Orchestrator.run() → Executor → Report

Target chat flow (Phase 1 — dispatcher value within the streaming loop):
  user message + history → tool-advisor scopes runtime → RiskGatedToolRuntime wraps it
    → converse_stream (tokens + tool events stream out) → each tool call gated at runtime
    (low-consequence executes; high-consequence → Proposal)
```

### The tool-name enabler bug

The `:` in Liberado's `<mcp>:<tool>` naming convention is rejected by the DeepSeek/OpenAI API
(`tools[].function.name` must match `^[a-zA-Z0-9_-]+$`). Any tool-bearing request returns HTTP
400 before the model sees the tools. This must be fixed before any dispatcher-chat integration
work — otherwise the model never sees the tools it's supposed to route over.

---

## Work Breakdown

Work is sequenced in five slices. Each slice is a coherent, shippable increment with its own tests
and smoke-validation.

### Slice 1 — Fix MCP tool-name `:` bug (enabler)

**Why:** Every tool-bearing request is currently broken. This must ship before any further MCP work.

**What:**
- In `crates/provider-deepseek/src/`, add a tool-name sanitizer that maps `mcp:tool` → a valid
  name (`mcp__tool`) during the completion request serialization, and reverses the mapping on the
  model's tool-call response.
- The sanitized name only exists in the JSON payload sent to the API; all internal code
  (`mcp_of()`, `ToolInvocation`, the MCP runtime) keeps the `:` convention.
- Implementation approach: a `SanitizedTools` struct holding the `Vec<ToolDef>` with sanitized
  names and a `reverse: HashMap<String, String>` for translating responses back. Construct at
  request time, consume at response time.
- **Bijection requirement:** the reverse map translates the model's tool-call response back to the
  original `:` name, so the sanitizer must be a bijection within a request. If two distinct tool
  names sanitize to the same valid string (e.g. `a:b` and `a_b` both → `a__b` under a naive
  replacement, or any collision the mapping admits), disambiguate deterministically (e.g. append a
  numeric suffix) so every sanitized name maps back to exactly one original. A collision that
  silently overwrites a reverse-map entry would mis-route the model's call to the wrong tool.

**Files:**
- `crates/provider-deepseek/src/lib.rs` — the OpenAI-compatible API boundary
- Possibly a new `crates/provider-deepseek/src/sanitize.rs`

**Tests:**
- Unit: sanitize `tasks-mcp:create` → valid pattern, round-trip back
- Unit: tool-call response with sanitized name is translated back to original
- Unit: already-valid names pass through unchanged
- Unit: collision case — two distinct names that sanitize to the same string are disambiguated, and
  each still reverse-maps to its own original (bijection holds)
- Integration: a `ChatSessions` with a mock MCP that returns tools with `:` names, driven through
  a real `DeepSeekProvider` (needs `DEEPSEEK_API_KEY` in CI)

**Depends on:** nothing (standalone fix)

---

### Slice 2 — Insert the dispatcher into the streaming chat loop (tool-advisor + runtime guards)

**Why:** Chat must gain the dispatcher's value — tool-advisor surfacing, safety guards,
sub-delegation — WITHOUT regressing two things it already has: token **streaming** (the shipped SSE
stack consumed by WebUI/TUI/CLI) and **multi-turn context**. Putting the dispatcher *in front of*
chat breaks both: it returns a non-streaming `Report`, and the dispatcher is deliberately
context-light (Decision 1), so a single-message goal loses the conversation's referents. So the
full-context **main agent keeps doing the actual execution** in its existing streaming loop (this
preserves context and prevents mission drift), and the dispatcher's value is decomposed and applied
within/around that loop.

**The dispatcher's three functions, applied to chat:**

**2a. Tool-advisor (token efficiency).** Before the turn runs, select the compact, *relevant* slice
of MCP tools to surface for this message (from the catalog) and scope the runtime to it — rather
than dumping every tool into context. This applies to the **direct/conversational path**, not just
subagents — it is the mesh-checkpoint-#1 token-efficiency win for the common case. A purely
conversational message surfaces no tools; a tool-relevant one surfaces the relevant MCP(s).
(Mechanism is a design choice: a cheap relevance pass — heuristic/embedding — or a small
classification call. The principle is "surface only the relevant slice"; the exact mechanism is
tunable per the token-efficiency pillar's "empirical testing finds the balance.")

**2b. Runtime guards (safety, where chat actually needs it).** Chat tool calls are **adaptive** —
the model decides them mid-conversation — so the dispatcher's *pre-flight* guards (which only see a
goal/seed) do not cover them. Wrap the scoped runtime in a `RiskGatedToolRuntime` that applies the
same deterministic guards (capability / consequence / magnitude) to each *real* call: low-consequence
runs; a high-consequence call is downgraded to a **Proposal** (Decision 11) instead of executing,
and the agent tells the user (streamed). This is the "runtime tool gating" already on the roadmap,
and it is where chat's real safety and the `Propose` path live. Capability scoping (2a) is the
safety floor; this consequence/magnitude gate is the completion. (Honest scope note: until 2b lands,
chat tool calls are guarded only by capability scoping, not by the consequence/magnitude gate.)

**2c. Sub-delegation (available for big work).** When a sub-goal is large, multi-step, or
context-heavy, the agent hands it to a capability-narrowed **subagent** (the disjoint-context path —
Decision 1's token-efficiency partition), which returns a `Report` folded back into the conversation
as a streamed summary. v1: expose this as a tool the conversational model can invoke (or a
classification for clearly-delegable goals) — do NOT force a classify on every turn.

**The chat turn flow:**
1. User message + full conversation history (from the store, as today).
2. Tool-advisor (2a) selects the relevant tool slice → scope the runtime → wrap in
   `RiskGatedToolRuntime` (2b).
3. Run the **streaming** `converse_stream` over the full history with the scoped + guarded runtime —
   tokens and `tool`/`tool_result` events stream to the client exactly as today.
4. Each tool call is gated at the runtime: low-consequence executes; high-consequence → a
   `proposals/<id>.md` artifact + a streamed "I've prepared a proposal for your approval."
5. Sub-delegation (2c) is available for big sub-goals.

**What this preserves/delivers:** streaming (no regression), multi-turn context (the loop holds the
history), token efficiency (tool-advisor scoping on the common path), real safety (runtime guards +
Propose), and sub-delegation.

**2d. Capability + correlation (unchanged from the original plan).** Chat uses the same grants as
the daemon for v1, with a `chat_capabilities` seam for later narrowing. Each turn mints a
correlation id (`chat:<session_ulid>:<turn_index>`) attached to tool-call provenance, so
chat-originated writes are distinct in the audit log.

**Files:**
- `crates/main-agent/src/sessions.rs` — `ChatSessions` keeps the streaming `converse_stream` loop;
  gains the tool-advisor selection step and wraps the runtime in `RiskGatedToolRuntime`
- `crates/mcp/` — new `RiskGatedToolRuntime` (wraps a `ToolRuntime`, applies
  capability/consequence/magnitude guards per call, emits a Proposal on a high-consequence call)
- `crates/server/src/lib.rs` — `build_chat()` constructs the tool-advisor + guarded runtime; the
  streaming endpoints are unchanged
- `crates/bootstrap/src/lib.rs` — share catalog/capability construction with the chat path
- `crates/main-agent` — sub-delegation exposed to the conversational loop (tool or option)

**Tests:**
- Unit: tool-advisor selects the relevant MCP slice for a tool-relevant message; surfaces nothing
  for pure chat
- Unit: `RiskGatedToolRuntime` runs a low-consequence call; downgrades a high-consequence call to a
  Proposal (no execution); rejects an out-of-capability call
- Unit: multi-turn context preserved — turn 2 resolves a referent from turn 1 (mock provider)
- Integration: streaming preserved end-to-end — `token`/`tool`/`tool_result`/`done` events still
  flow through the dispatched chat path
- Live smoke: `liberado chat` through a real provider + MCP, streaming reply with tool use; a
  high-consequence tool request produces a proposal instead of acting

**Depends on:** Slice 1 (tool-name fix)

---

### Slice 3 — Live capability catalog + on-demand tool surfacing (mesh checkpoint #1)

**Why:** The catalog is currently a static `Vec<McpDescriptor>` built from `topology.toml` at boot.
This means:
- Tools cannot be added/removed while the daemon runs (no hot-reload for Phase 2's `ProposeMcp`)
- The TUI and WebUI cannot query "what tools exist right now?"
- The dispatcher sees a stale catalog if an MCP server adds tools after initialization

The live catalog is also the first concrete bus-native component — it is a single source of truth
that multiple consumers (dispatcher, TUI, WebUI, Phase 2's self-extension) query independently.

**What:**

#### 3a. `CapabilityCatalog` type in `liberado-common`

```rust
/// A live, queryable capability catalog. Multiple consumers (dispatcher, TUI, WebUI)
/// can independently query it; updates are propagated via a watch channel.
pub struct CapabilityCatalog {
    inner: Arc<RwLock<CatalogState>>,
    // Notify subscribers when the catalog changes (MCP added/removed/tools changed).
    updated: tokio::sync::watch::Sender<()>,
}

struct CatalogState {
    mcps: Vec<McpDescriptor>,
    last_updated: Instant,
}
```

- `register(mcp: McpDescriptor)` — add/update an MCP entry
- `deregister(name: &str)` — remove an MCP entry
- `descriptors() -> Vec<McpDescriptor>` — snapshot for the dispatcher
- `subscribe() -> watch::Receiver<()>` — for consumers that want to react to catalog changes

#### 3b. Catalog population at boot

At boot, `McpRegistry` (from `liberado-mcp`) connects to each enabled MCP server, fetches its
tool list, and registers the server as a `McpDescriptor` in the catalog. The catalog's
descriptors are the **same** as today's static config entries, but now they are populated from
live connections.

#### 3c. On-demand tool surfacing

The token-efficiency pillar says "tools surfaced on-demand, never dumped into context." Today the
executor's `catalog()` returns the full tool list, and the model sees all tools in every request.
Phase 1 introduces the **lazy-loading pattern**: the catalog knows which tools exist, but only the
relevant slice is surfaced in the execution context.

On-demand surfacing must apply to the **direct/conversational path** — via Slice 2's tool-advisor,
which selects the relevant slice for each chat message — not only to subagents. The direct path is
the common case, so scoping it is the actual token-efficiency win; surfacing only the subagent's
narrowed tools while still dumping the full catalog into every conversational turn would miss the
point of the pillar.

Concretely: the orchestrator already scopes the runtime to `allowed_mcps` (via
`TurbomcpRuntimeFactory.runtime_for()`), and Slice 2's tool-advisor scopes the conversational
runtime the same way. The catalog lives at the dispatcher/advisor level (compact descriptions, not
full schemas). The executor's tool list comes from the scoped runtime, not the global catalog. This
is already partially true — the Phase 1 change is formalizing the catalog as a shared type with the
subscribe/watch pattern.

#### 3d. TUI/WebUI query surface

Expose `GET /api/catalog` that returns the live catalog contents (MCP names, descriptions,
consequence levels, per-MCP tool names — but NOT full tool schemas). The TUI uses this to show
"available tools: tasks-mcp (3 tools), wiki-mcp (1 tool)".

**Files:**
- `crates/common/src/catalog.rs` — new: `CapabilityCatalog` type
- `crates/common/src/lib.rs` — re-export
- `crates/mcp/src/factory.rs` — `McpRegistry` populates the catalog on boot
- `crates/bootstrap/src/lib.rs` — assemble the catalog, inject it into the dispatcher and the API
  state
- `crates/server/src/api.rs` — `GET /api/catalog` endpoint
- `crates/server/src/state.rs` — `AppState` holds `Arc<CapabilityCatalog>`

**Tests:**
- Unit: `CapabilityCatalog` register/deregister/descriptors
- Unit: watch subscriber receives notification on register
- Unit: empty catalog → descriptors is empty
- Integration: boot with two MCPs → catalog lists both; boot with zero MCPs → catalog is empty

**Depends on:** Slice 1 (tool-name fix). The live catalog is foundational and largely independent —
Slice 2 can run against the existing static `catalog_from_config` (already built), and Slice 3
upgrades that to a live, queryable catalog. So this slice can land earlier or in parallel rather
than strictly after Slice 2; Slice 2 simply gives the live catalog a second concrete consumer beyond
the daemon once both are present.

---

### Slice 4 — Multi-MCP + parallel, capability-narrowed sub-delegation

**Why:** The system currently connects to a single MCP server. Hermes gap #4 is the ability to
connect to multiple MCPs simultaneously and dispatch subagents in parallel, each scoped to the
narrowest set of MCPs it actually needs. This is the differentiator: a single agent that can reach
tasks, a wiki, a code forge, and email through one conversation, safely.

**What:**

#### 4a. Multi-MCP connection at boot

The `McpRegistry` already supports multiple entries — registering each MCP by name with a
connector. The gap is that `TurbomcpRuntimeFactory` currently connects to a single server and
returns a single runtime. Multi-MCP needs a factory that can build a **composite runtime** spanning
multiple connected MCP servers.

```rust
/// A ToolRuntime that merges the catalogs of several connected runtimes and dispatches
/// invocations to the correct one based on the `mcp_of()` prefix.
pub struct MultiMcpRuntime {
    runtimes: HashMap<String, Box<dyn ToolRuntime>>,
    merged_catalog: Vec<ToolDef>,
}
```

`TurbomcpRuntimeFactory` gains a `multi: bool` flag (or is replaced by `MultiMcpFactory`). When
enabled, `runtime_for(allowed_mcps, provenance)` connects to every MCP in the registry whose name
is in `allowed_mcps`, builds a per-server `TurbomcpRuntime`, and merges them into a
`MultiMcpRuntime`.

#### 4b. Parallel sub-delegation

Today the orchestrator runs `DispatchSubagent` synchronously: one subagent, one report. Phase 1
adds a `dispatch_parallel()` variant (or a `spawn_subagent()` method) that:

1. The dispatcher decomposes the goal into independent **sub-goals** and classifies a
   `DispatchSubagent` for each
2. The orchestrator spawns one subagent **per sub-goal**, each with its own `Task`, a `ToolRuntime`
   scoped to exactly the MCPs that sub-goal needs (its capability narrowing), and a `Budget`
3. Subagents run in parallel via `tokio::spawn`
4. Results are collected and merged into a single `Report`

The decomposition is **goal-driven, not mechanical**: parallelize by independent sub-goal, not "one
subagent per MCP." A single sub-goal may legitimately touch several MCPs (e.g. read a wiki page and
file a task from it), and two sub-goals may both touch the same MCP. The unit of parallelism is the
independent sub-goal; each is then capability-narrowed to the MCPs it actually uses.

The parallelism is gated by `tuning.dispatch.max_concurrent_subagents` (already in the config
schema).

#### 4c. Capability narrowing enforcement

Decision 4: "a subagent gets `base ∩ narrowing` — never wider." The catalog's consequence metadata,
combined with the capability set from policy, ensures a subagent scoped to `tasks-mcp` cannot
invoke `email-mcp:send` even if the base agent holds that grant. The `ScopedToolRuntime` (already
implemented in `crates/mcp/src/factory.rs`) enforces this at the tool-call boundary: an
out-of-scope call is rejected before it reaches the MCP server.

**Files:**
- `crates/mcp/src/factory.rs` — `McpRegistry` gains `runtime_for(allowed_mcps, provenance)`
  producing a `MultiMcpRuntime` when multiple MCPs are configured
- `crates/mcp/src/runtime.rs` — new `MultiMcpRuntime` type
- `crates/orchestrator/src/lib.rs` — `dispatch_parallel()` method
- `crates/common/src/config.rs` — validate `max_concurrent_subagents` ≥ 1 (already in schema,
  may need enforcement at runtime)

**Tests:**
- Unit: `MultiMcpRuntime` correctly routes `tasks-mcp:create` to the tasks runtime and
  `wiki:search` to the wiki runtime
- Unit: `MultiMcpRuntime` with empty `allowed_mcps` returns the full merged catalog
- Unit: `MultiMcpRuntime` rejects a call to an MCP not in the scoped set
- Unit: `dispatch_parallel()` with 2 independent sub-goals spawns 2 subagents (each
  capability-narrowed to the MCPs its sub-goal needs) and merges reports
- Unit: a single sub-goal that touches two MCPs is run by one subagent scoped to both (not split
  per-MCP)
- Unit: `dispatch_parallel()` gated by `max_concurrent_subagents` — exceeding the cap queues or
  rejects
- Integration: boot with 2 MCP servers (e.g., tasks + deepwiki), dispatch a goal that decomposes
  into sub-goals touching both, verify the subagents run in parallel and produce a merged outcome

**Depends on:** Slice 3 (live catalog provides the MCP descriptor list the dispatcher routes over)

---

### Slice 5 — TUI client extraction + `chat-client-contract` crate

**Why:** The modularity proof: extract the shared SSE chat contract into a standalone crate, then
build the TUI client against that contract. The TUI depends on nothing from the agent internals —
just the contract crate.

**What:**

#### 5a. Extract `chat-client-contract` crate

Pull the SSE event types (`AgentEvent`, `session`, `token`, `tool`, `tool_result`, `done`,
`failed`) and the `ChatClient` trait into a new `crates/chat-client-contract/` (or
`crates/chat-contract/`).

```rust
#[async_trait]
pub trait ChatClient {
    async fn send(&self, message: &str, session: Option<Ulid>) -> Result<ChatResponse, ChatError>;
    async fn stream(
        &self,
        message: &str,
        session: Option<Ulid>,
    ) -> Result<Pin<Box<dyn Stream<Item = ChatEvent> + Send>>, ChatError>;
}
```

Note the boxed stream return type: `#[async_trait]` rewrites each method to return a boxed future,
which cannot carry an unboxed `impl Stream` return — so `stream()` must return a named stream type
(`Pin<Box<dyn Stream<Item = ChatEvent> + Send>>`). The alternative is to drop `#[async_trait]` for
that one method and hand-write it as a non-`async fn` returning `impl Future<…, impl Stream…>`; the
boxed-stream form is simpler and is what this plan specifies.

The `liberado chat` CLI client and the TUI depend only on this crate. The server depends on it
(for the types it emits), but the contract is owned by neither.

#### 5b. Update `liberado-tui` to use the contract

The existing TUI scaffold (`crates/tui/`) currently depends on `liberado-server` types. Rebase it
on `chat-client-contract`. The TUI:
- Connects to `liberado serve` over HTTP/SSE using the contract's `ChatClient`
- Queries `GET /api/catalog` from Slice 3 to show available tools
- Streams tokens, renders tool calls as collapsible sections
- Displays the conversation tree (future: DAG branching from the TUI ROADMAP)

#### 5c. TUI-specific features (from `crates/tui/ROADMAP.md`)

Already planned: loading indicators, markdown rendering, status bar/header, conversation search,
theming, stop button (`Ctrl+S`), mouse support. These are TUI implementation details that don't
affect the Phase 1 architecture — they are tracked in the TUI's own roadmap.

**Files:**
- `crates/chat-client-contract/src/lib.rs` — new crate: `ChatClient` trait, event types
- `crates/chat-client-contract/Cargo.toml` — dependencies: `liberado-conversation-store` (Ulid),
  `serde`, `async-trait`
- `crates/server/src/api.rs` — emit event types from `chat-client-contract` instead of
  `liberado-executor::AgentEvent`
- `crates/cli/src/chat_client.rs` — dep on `chat-client-contract`, not server internals
- `crates/tui/` — dep on `chat-client-contract` only; remove any dep on `liberado-server`
- `crates/tui/Cargo.toml` — pull in `ratatui`, `crossterm`, `reqwest`, `tokio`, `chat-client-contract`

**Tests:**
- The existing `liberado chat` CLI client continues to work — it is the first consumer of the
  extracted contract
- TUI: connect to a running `liberado serve`, send a message, see the reply rendered

**Depends on:** Slice 1 (tool-name fix) for end-to-end use. The **core contract extraction (5a) does
not depend on Slice 3** — the catalog display (`GET /api/catalog`) is a feature the TUI layers on,
not a dependency of the extracted `ChatClient`/event types. So 5a and the TUI rebase can start
earlier or in parallel once Slice 1 lands; only the catalog-display piece (5b's "available tools"
view) needs Slice 3.

---

## Dependency Graph

```
Slice 1 (tool-name fix)
  └─► Slice 2 (chat dispatcher routing)
        └─► Slice 3 (live catalog)
              └─► Slice 4 (multi-MCP + parallel sub-delegation)
              └─► Slice 5 (TUI + contract extraction)
```

Slices 4 and 5 are independent of each other and can proceed in parallel once Slice 3 is done.

**Less strictly downstream than drawn:** Slice 3 (live catalog) and Slice 5 (contract extraction)
are drawn under Slice 2 for tidiness, but neither hard-depends on it. Slice 3 is foundational —
Slice 2 can use the existing static `catalog_from_config` and Slice 3 upgrades it to live — and
Slice 5's core contract extraction depends only on the SSE event types, not the catalog. Both can
start earlier or in parallel once **Slice 1** lands. Only Slice 4 (which routes over live MCP
descriptors) and Slice 5's catalog-display feature genuinely want Slice 3 in place first.

---

## Architectural Decisions Specific to Phase 1

### A. Streaming is preserved

**Decision:** The chat reply is the streamed conversational answer — tokens plus `tool`/`tool_result`
events — not a non-streaming `Report`. The dispatcher's value is added *within* the streaming loop
(tool-advisor scoping + runtime guards), never by replacing the loop with a classify→Report cycle.

**Rationale:** Streaming is already shipped and consumed by every client (WebUI/TUI/CLI over the SSE
stack). Putting the dispatcher in front of chat would return a non-streaming `Report` and, because
the dispatcher is deliberately context-light (Decision 1), would also drop the conversation's
multi-turn referents. Both are regressions. Instead the full-context main agent keeps executing in
its existing `converse_stream` loop — preserving streaming and context — while the dispatcher's
value is decomposed into a tool-advisor (which scopes the runtime before the turn) and a
`RiskGatedToolRuntime` (which gates each adaptive call). The user sees tokens stream as today; a
high-consequence tool call yields a streamed proposal message instead of silently executing.

### B. Chat uses the same dispatcher instance as the daemon

**Decision:** The chat path and the daemon's reactive path share one `Dispatcher` and one
`Orchestrator` instance (same `Arc<dyn Provider>`, same `Tuning`, same `CapabilitySet`).

**Rationale:** Two dispatcher instances would need two providers (doubling cost) and would diverge
in guard configuration. A single dispatcher with distinct `DispatchRequest` construction (chat sets
`reaction_depth: 0`, daemon sets `reaction_depth: 1`) gives consistent classification + guards with
one provider call.

**Note:** The same deterministic guards (capability / consequence / magnitude) now apply in **both**
paths, just at different boundaries: the daemon enforces them **pre-flight** over a goal/seed, while
chat enforces them at the **runtime boundary** via `RiskGatedToolRuntime` (Slice 2b), where the
adaptive tool calls actually happen. Sharing the dispatcher keeps the guard *definitions* single-
sourced even though the *application point* differs by path.

### C. The `EventBus` trait is introduced as the chat→dispatcher seam

**Decision:** The wiring from chat to dispatcher (Slice 2) is the first bus-native seam per
Decision 18. Create a minimal `EventBus` trait (`post()` + `subscribe()`) in `liberado-common`, and
have the chat path post a `GoalEvent` that the dispatcher consumes, rather than calling
`dispatcher.dispatch()` directly.

**Rationale:** This is the smallest step toward the [mesh vision](../../ideas/archive/meshify.md). It's one
trait with one implementation (the `tokio::sync::broadcast` in-process bus), and it immediately
proves the pattern: chat is a producer, dispatcher is a consumer, neither holds a direct reference
to the other. The daemon's watch loop can join the same bus in a follow-up (it currently calls
`dispatcher.dispatch()` directly — that call site migrates to the bus when the daemon is touched).

**Sequencing (the explicit default):** Land the chat integration with **direct calls first**, then
introduce the `EventBus` as an **isolated follow-up** once the integration is green. Do not couple
the new bus abstraction to the integration work — getting tool-advisor + runtime guards into the
streaming loop is the Slice 2 deliverable, and it should not block on (or be destabilized by) the
first design iteration of the bus trait. The direct path (the advisor scoping the runtime, the
agent's loop driving the guarded runtime) is the *pragmatic* primary; the bus is the *ideal* per
Decision 18, added afterward as a seam, not a gate. The safety properties depend on the runtime
guards, not on the bus, so the bus can be iterated separately without risk to the integration.

### D. Chat reply format (streamed conversational answer)

**Decision:** The chat reply is the **streamed conversational answer** — tokens plus tool events —
produced by the main agent's `converse_stream` loop. It is not a non-streaming structured `Report`.

**Rationale:** Streaming and multi-turn context are the properties chat already has and must keep
(see Decision A). The dispatcher's value is folded into that loop rather than replacing it, so the
default reply is conversational prose with inline tool calls. The dispatcher's outcomes map onto the
stream as follows:
- **Direct / conversational** → streamed prose plus `tool`/`tool_result` events, exactly as today.
- **Delegated** → a subagent's `Report { summary, artifacts, follow_up }` is folded back into the
  conversation as a **streamed summary**, with artifacts surfaced as named outputs.
- **Clarify** → the model asks the clarifying question naturally in the stream (or a low-confidence
  path surfaces it), inviting the user to clarify.
- **Propose** → a streamed "I've prepared a proposal `proposals/<id>.md` for your approval," with
  the proposal artifact written for out-of-band review (the `RiskGatedToolRuntime` produces it on a
  high-consequence call).

The conversational chat loop (model + tools, adaptive) stays the primary execution path; the
dispatcher's value is applied within and around it, not as a replacement for it.

---

## Testing Strategy

### Per-slice unit tests

Every slice adds tests in the crate where the logic lives, following the existing pattern:
- Dispatcher: mock provider + classified decisions → guard evaluation
- Orchestrator: mock `RuntimeFactory` + mock provider → `Disposition`
- Main-agent: tool-advisor selects the relevant tool slice; multi-turn context preserved across
  turns; `RiskGatedToolRuntime` runs low-consequence calls and downgrades high-consequence ones to a
  Proposal
- MCP: `RiskGatedToolRuntime` per-call gating (capability / consequence / magnitude)
- MCP: `MultiMcpRuntime` tested with per-server channel transports (the existing in-process
  channel test pattern)
- Catalog: `CapabilityCatalog` register/deregister/subscribe
- Contract: `ChatClient` trait tested with a mock SSE stream

### Integration tests

| Test | What it verifies |
|------|-----------------|
| `chat_streaming_through_advisor_e2e` | User message → tool-advisor scopes runtime → `RiskGatedToolRuntime` → `converse_stream`; `token`/`tool`/`tool_result`/`done` events flow and multi-turn context resolves a referent. Uses `JsonlStore` on a temp dir for conversation persistence. |
| `chat_high_consequence_proposal` | A high-consequence chat tool call is downgraded to a `proposals/<id>.md` artifact + streamed proposal message instead of executing. |
| `multi_mcp_subagent_merge` | Two mock MCP servers (channel transport), independent sub-goals route across both, subagents run in parallel (each capability-narrowed), reports merge. |
| `live_catalog_population` | Boot with 2 MCP configs → catalog has 2 entries → `GET /api/catalog` returns both. |
| `tool_name_sanitize_roundtrip` | `mcp:tool` → sanitized → API call → response → unsanitized. |

### Live smoke

The existing smoke test pattern from `AGENTS.md` extends to verify:
1. `liberado serve` boots with a configured MCP server
2. `liberado chat "add milk to the shopping list"` → tool-advisor surfaces the relevant MCP → the
   streaming loop runs the tool → reply streams and confirms the action
3. A high-consequence request (e.g. "delete the whole list") → `RiskGatedToolRuntime` downgrades it
   to a streamed "I've prepared a proposal for your approval" instead of executing
4. `GET /api/catalog` returns the live tool list
5. A `Clarify` scenario (ungranted MCP, or low-confidence goal) surfaces a clarification question
   instead of executing

### Eval regression

`crates/eval/` (the routing/safety eval suite) must continue to pass. The eval tests the
dispatcher in isolation against real model inference; Phase 1 must not regress the safe-default
rate or the UNSAFE-acts metric. Run `cargo run -p liberado-eval` before and after Phase 1 to
confirm.

---

## File Manifest (expected changes)

| File | Slice | Change |
|------|-------|--------|
| `crates/provider-deepseek/src/sanitize.rs` | 1 | New: tool-name sanitizer |
| `crates/provider-deepseek/src/lib.rs` | 1 | Wire sanitizer into completion request/response |
| `crates/main-agent/src/sessions.rs` | 2 | `ChatSessions` keeps streaming `converse_stream`; adds tool-advisor + `RiskGatedToolRuntime` wrap |
| `crates/main-agent/src/lib.rs` | 2 | Sub-delegation exposed to the conversational loop (tool or option) |
| `crates/mcp/` | 2 | New `RiskGatedToolRuntime` — wraps a `ToolRuntime`, applies capability/consequence/magnitude guards per call, emits a Proposal on a high-consequence call |
| `crates/server/src/lib.rs` | 2 | `build_chat()` constructs the tool-advisor + guarded runtime; streaming endpoints unchanged |
| `crates/bootstrap/src/lib.rs` | 2, 3 | `configure_chat()` or shared catalog/capability construction |
| `crates/common/src/catalog.rs` | 3 | New: `CapabilityCatalog` type |
| `crates/common/src/lib.rs` | 3 | Re-export catalog types |
| `crates/mcp/src/factory.rs` | 3, 4 | `McpRegistry` populates catalog; `runtime_for()` supports multi-MCP |
| `crates/mcp/src/runtime.rs` | 4 | `MultiMcpRuntime` composite runtime |
| `crates/orchestrator/src/lib.rs` | 4 | `dispatch_parallel()` method |
| `crates/server/src/api.rs` | 3, 5 | `GET /api/catalog`; emit `chat-client-contract` event types |
| `crates/server/src/state.rs` | 3 | `AppState` holds `Arc<CapabilityCatalog>` |
| `crates/chat-client-contract/src/lib.rs` | 5 | New crate: `ChatClient` trait, event types |
| `crates/chat-client-contract/Cargo.toml` | 5 | New crate manifest |
| `crates/cli/src/chat_client.rs` | 5 | Dep on `chat-client-contract`, not server internals |
| `crates/tui/Cargo.toml` | 5 | Dep on `chat-client-contract` only |
| `crates/tui/src/` | 5 | Rebase on extracted contract + catalog API |
| `Cargo.toml` (workspace) | 5 | Add `chat-client-contract` to workspace members |

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Tool-name sanitization leaks sanitized names into user-facing output | Low | Medium | Sanitization is a request-layer transform; the reverse map is applied before any string reaches the model's response or the tool-call path. Unit test for every code path. |
| Dispatcher latency makes chat feel slow | Low | Medium | Largely resolved by the design: streaming is preserved, so tokens start arriving immediately rather than after a classify→Report cycle, and the plan does **not** force a classify on every turn — the tool-advisor is a cheap relevance pass (heuristic/embedding or a small call) and pure conversational turns surface no tools. The only added pre-turn cost is the advisor's selection step; instrument it and the runtime-guard checks for profiling. |
| EventBus trait design is wrong on first attempt | Low | Low | Decoupled by sequencing: Slice 2 lands with direct calls first, and the `EventBus` is an isolated follow-up (Decision C). The bus is a seam, not a gate — the safety properties depend on the runtime guards, not the bus — so its design can be iterated separately without destabilizing the integration. |
| Multi-MCP connection failures at boot block the daemon | Medium | Medium | Graceful degradation: a failed MCP connection is logged, the MCP is excluded from the catalog, and the daemon boots with the surviving MCPs. Already the pattern for `build_chat()` — extend to `McpRegistry`. |
| Parallel sub-delegation OOM from unbounded concurrency | Low | High | `max_concurrent_subagents` cap (from `tuning.toml`) is enforced in the orchestrator. Default cap is 2; the `tokio::spawn` is bounded by a semaphore. |

---

## Definition of Done

Phase 1 is complete when all of the following are true:

1. **Tool-name fix deployed.** Running `liberado chat` with an MCP server configured produces tool
   calls (not HTTP 400 errors). Validated by live smoke.
2. **Chat routes through the dispatcher's tool-advisor + runtime guards within the streaming loop.**
   Every user message runs the main agent's `converse_stream` loop with a tool-advisor-scoped
   runtime wrapped in `RiskGatedToolRuntime`. **Streaming and multi-turn context are preserved** —
   `token`/`tool`/`tool_result`/`done` events still flow and a later turn resolves a referent from
   an earlier one. A **high-consequence chat tool call yields a Proposal** (`proposals/<id>.md`)
   rather than executing. Validated by tests asserting streamed events end-to-end, multi-turn
   referent resolution, and the high-consequence→Proposal downgrade.
3. **Live catalog is queryable.** `GET /api/catalog` returns the live list of MCP servers and
   their tool counts. The catalog updates when an MCP connects/disconnects. Validated by unit
   tests and the API endpoint.
4. **Multiple MCP servers can be configured and used.** Two [[mcps]] entries in `topology.toml`
   produce a `MultiMcpRuntime`. A goal that touches both MCPs routes correctly. Validated by
   integration tests with two in-process mock MCP servers.
5. **Parallel sub-delegation works.** A `DispatchSubagent` that names two MCPs spawns two
   subagents concurrently, bounded by `max_concurrent_subagents`. Validated by an orchestrator
   unit test with a mock `RuntimeFactory` that records parallel spawns.
6. **TUI client is standalone.** `crates/tui/` depends on `chat-client-contract`, not
   `liberado-server`. The TUI connects to a running `liberado serve` and displays the catalog and
   chat replies. Validated by building the TUI and running a manual smoke (the TUI's own tests
   cover interaction logic).
7. **No regressions.** `cargo test --workspace` is green. `cargo run -p liberado-eval` passes
   (safe-default rate unchanged, UNSAFE-acts at zero). The existing `liberado chat` CLI and Web UI
   continue to function.
8. **AGENTS.md updated.** The build/run instructions reflect the chat-dispatcher path and the new
   `chat-client-contract` crate.
