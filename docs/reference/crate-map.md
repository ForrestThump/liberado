# Crate map

> **Generated file - do not edit.** Regenerate with `powershell -File scripts/gen-crate-map.ps1`.
> Source of truth: each crate's `Cargo.toml` (`description` + `[package.metadata.liberado] role`).
> Layer semantics and dependency rules: [contracts.md](../architecture/contracts.md) and
> `crates/test-support/tests/layer_rules.rs` (the same role tags, mechanically enforced).

42 workspace crates as of 2026-07-13.

## foundation

The bottom layer: vocabulary and narrow-waist traits. Depends on nothing above itself.

| Crate | Internal deps | Description |
|---|---|---|
| [`liberado-common`](../../crates/common/) | *none* | Shared types for the Liberado system: capabilities/zones, write provenance, events, dispatch decisions, proposals, and model profiles. |
| [`liberado-config-loader`](../../crates/config-loader/) | `liberado-common` | ConfigSource trait + ChainLoader for layered Liberado configuration loading. |
| [`liberado-notify`](../../crates/notify/) | *none* | A minimal, pluggable notification sink for events a human should know about even when nothing else in the daemon's own surfaces is open in front of them right now — the motivating case is an unattended (cron-triggered, Phase 3) proposal nobody's watching for. Telegram is the first implementation (free, mature); the Notifier trait exists so it isn't the only one. |
| [`liberado-provider`](../../crates/provider/) | *none* | The provider-agnostic inference interface for Liberado: a narrow async `Provider` trait (tool-calling + structured output) plus a scriptable mock for deterministic tests. |

## client

Front-end building blocks, liftable into any UI without dragging the system along.

| Crate | Internal deps | Description |
|---|---|---|
| [`chat-client-contract`](../../crates/chat-client-contract/) | *none* | Shared HTTP/SSE wire DTOs + the SseDecoder incremental parser -- the one contract every chat surface (TUI, WebUI, CLI) speaks. |
| [`liberado-commands`](../../crates/liberado-commands/) | `liberado-theme` | Shared slash-command parser and handlers for Liberado chat clients (TUI, WebUI, CLI) |
| [`liberado-markdown`](../../crates/markdown/) | *none* | Lightweight Markdown parser for Liberado — UI-agnostic blocks and inline spans consumed by ratatui, Dioxus, and terminal output. |
| [`liberado-theme`](../../crates/theme/) | *none* | Shared theme definitions for Liberado UIs — color tokens consumed by ratatui, Dioxus/CSS, and terminal output. No UI dependency. |

## kernel

The orchestration engine: decide/act loops, sessions, capability plumbing.

| Crate | Internal deps | Description |
|---|---|---|
| [`liberado-config`](../../crates/config/) | `liberado-common`, `liberado-config-loader` | Config loading (Decision 14): resolve a config directory, read the per-section TOML files, assemble and validate a Config, plus the path-resolution helpers (mcp install dir, operational data dir, runtime-gating context) built on it. Deliberately dependency-light — no daemon/mcp/dispatcher/orchestrator — so tools that only need config/path resolution (e.g. liberado-mcp-forge) don't have to pull in the whole assembly stack. |
| [`liberado-cron`](../../crates/cron/) | `liberado-common` | Cron as an EventSource (Decision 18/19): a schedule fires on its own timer and produces the same standardized Event vault-watch does, so the daemon reacts to it identically. Vault-agnostic by construction — no liberado-vault dependency — the concrete proof the core doesn't need the vault. |
| [`liberado-dispatcher`](../../crates/dispatcher/) | `liberado-common`, `liberado-config-loader`, `liberado-provider` | The Liberado dispatcher (Decision 1): the out-of-band router that classifies a goal into a DispatchDecision, then applies the deterministic downgrade-only guard pipeline (Decision 6) that engineers safety regardless of classifier error. |
| [`liberado-executor`](../../crates/executor/) | `liberado-common`, `liberado-notify`, `liberado-provider`, `liberado-scratchpad` | The Liberado agent execution engine: a bounded, adaptive tool loop that drives a Provider through real tool calls until it files a typed Report (delegated work) or answers in prose (conversational). |
| [`liberado-main-agent`](../../crates/main-agent/) | `liberado-common`, `liberado-conversation-store`, `liberado-dispatcher`, `liberado-executor`, `liberado-mcp`, `liberado-provider`, `liberado-session` | The conversational main agent: a multi-turn Conversation that carries context across turns and drives the executor's tool-calling loop. The thing a chat UI talks to. |
| [`liberado-mcp`](../../crates/mcp/) | `liberado-common`, `liberado-provider`, `liberado-executor` | The production `ToolRuntime` for Liberado: a turbomcp-client-backed adapter that exposes an MCP server's tools to the executor and injects write provenance into each call's `_meta` so tool-mediated vault writes are self-attributed (loop-broken). |
| [`liberado-orchestrator`](../../crates/orchestrator/) | `liberado-common`, `liberado-notify`, `liberado-provider`, `liberado-session`, `liberado-executor` | Bridges a dispatcher DispatchDecision to an execution: builds the task + write provenance, runs the executor's agent loop over a ToolRuntime, and returns a Report (or surfaces a Clarify). Decoupled from MCP connection management via the RuntimeFactory trait. |
| [`liberado-provider-openai-compat`](../../crates/provider-openai-compat/) | `liberado-provider` | A single, config-driven Provider implementation for any OpenAI-compatible chat-completions API (DeepSeek, OpenRouter, and future backends like OpenAI direct/Groq/Together) — replaces what used to be one hand-copied crate per backend. |
| [`liberado-scratchpad`](../../crates/scratchpad/) | `liberado-provider` | Per-execution scratchpad (todo-list) tool for the Liberado engine — engine-injected, not an MCP. |
| [`liberado-session`](../../crates/session/) | `liberado-common` | The session kernel (D7): GoalSpec, SessionGrant, Visibility, the SessionEvent envelope, the GoalSessionHub, the DomainPackRunner trait, and the SessionRecordStore seam the converged store implements. Surfaces are clients of this contract — not loop owners. |

## store

Persistent and shared information: vault, conversations, memory, search.

| Crate | Internal deps | Description |
|---|---|---|
| [`liberado-chat-search`](../../crates/chat-search/) | `liberado-conversation-store`, `liberado-session-store`, `liberado-provider` | Full-text and regex search over Liberado session history — it scans the converged session store's JSONL logs directly. Query parsing, per-file scanning, snippet extraction, shared by liberado-server's REST endpoint and the chat-search MCP tool so search logic exists exactly once. |
| [`liberado-conversation-store`](../../crates/conversation-store/) | `liberado-provider` | The Decision-17 conversation CONTRACT: the ConversationStore trait plus the message-node DAG types (id, parent_id, Author) every session transcript is made of. No implementation lives here — liberado-session-store implements it. |
| [`liberado-memory-store`](../../crates/memory-store/) | `liberado-common`, `liberado-vault` | Vault-backed general (facts/preferences) and procedural (tool-selection guidance) memory stores shared by liberado-memory-mcp and the dispatcher. Cleartext markdown notes are the source of truth; each store also maintains a turbovault-vector HNSW index for semantic recall. Native Rust — no external mem0 dependency. |
| [`liberado-session-store`](../../crates/session-store/) | `liberado-common`, `liberado-conversation-store`, `liberado-provider`, `liberado-session` | The converged Session store (D7): one append-only JSONL log per session holding both message nodes (the DAG) and pack events. Two typed lenses — ConversationStore and SessionRecordStore — over one engine. |
| [`liberado-vault`](../../crates/vault/) | `liberado-common` | Liberado's thin adapter over Turbovault: provenance-tagged writes (Decision 5) and consumer-side hash-join attribution for loop-breaking. The single place the upstream-dependency fallbacks (concurrency spec §8.1) are isolated. |

## pack

Domain packs (coding first). Never sit beneath kernel/config/store layers.

| Crate | Internal deps | Description |
|---|---|---|
| [`liberado-coder-agent`](../../crates/coder-agent/) | *none* | Coding domain pack: Liberado goal-session backend over Executor + coding tools (not the session kernel) |
| [`liberado-coder-core`](../../crates/coder-core/) | `liberado-common` | Provider-agnostic contracts for Liberado's Rust-native coding backend: tasks, backend trait, sandbox specs, events, reports, and traces. |
| [`liberado-coder-runner`](../../crates/coder-runner/) | *none* | Process boundary for the coding pack: the liberado-coder-run subprocess bridge nested consumers (PR factory) drive over JSON. |
| [`liberado-coder-sandbox`](../../crates/coder-sandbox/) | *none* | Workspace and command sandbox abstractions for Liberado's Rust-native coder |
| [`liberado-coder-tools`](../../crates/coder-tools/) | *none* | Executor ToolRuntime implementation for Liberado's Rust-native coder |
| [`liberado-dispatch-pack`](../../crates/dispatch-pack/) | `liberado-common`, `liberado-dispatcher`, `liberado-notify`, `liberado-orchestrator`, `liberado-session` | Domain pack that runs the dispatcher + orchestrator as a GoalSessionHub pack — the one-execution-engine convergence (E2). |

## service

Out-of-process adapters: MCP servers, bots, the forge.

| Crate | Internal deps | Description |
|---|---|---|
| [`liberado-chat-search-mcp`](../../crates/chat-search-mcp/) | `liberado-chat-search`, `liberado-config` | MCP server exposing conversation history search as a tool, so the dispatcher can search chat history mid-reasoning (not just the human, via the webui). Registered in topology.toml as a stdio MCP; built in-workspace, not a managed (cargo-installed-from-git) MCP. |
| [`liberado-mcp-forge`](../../crates/mcp-forge/) | `liberado-common`, `liberado-config` | Builds and installs Liberado MCP servers from git URLs via `cargo install --git`, wiring them to `McpTransport::Managed` entries in topology.toml by convention. |
| [`liberado-memory-mcp`](../../crates/memory-mcp/) | `liberado-common`, `liberado-config`, `liberado-vault`, `liberado-memory-store` | MCP server exposing general memory (user facts/preferences) and procedural memory (tool-selection guidance) as agent-callable tools, backed by liberado-memory-store. Registered in topology.toml as a stdio MCP; built in-workspace, not a managed (cargo-installed-from-git) MCP. Replaces the old liberado-tool-helper-mcp, which proxied every call over HTTP to an external mem0 service. |
| [`liberado-telegram-approvals`](../../crates/telegram-approvals/) | `liberado-common`, `liberado-config-loader`, `liberado-provider`, `liberado-vault` | A Telegram bot that turns Approve/Reject/Revise button taps into pure-code proposal frontmatter edits — Approve/Reject never touch an LLM; Revise hands free text to the shared provider only to redraft a proposal's content, which still requires a fresh human tap to execute. |

## surface

UIs. Clients of the wire contract only - enforced by layer_rules.rs.

| Crate | Internal deps | Description |
|---|---|---|
| [`liberado-tui`](../../crates/tui/) | `liberado-theme`, `liberado-markdown`, `liberado-commands`, `chat-client-contract` | ratatui TUI client for Liberado — chat pane, status line, reactions feed; drives the shared HTTP/SSE API (docs/reference/api.md) |
| [`liberado-webui`](../../crates/webui/) | `chat-client-contract`, `liberado-commands`, `liberado-theme` | Dioxus WASM web UI for Liberado — interfaces with the daemon via HTTP API |

## root

Composition roots: the only crates allowed to see everything.

| Crate | Internal deps | Description |
|---|---|---|
| [`liberado-bootstrap`](../../crates/bootstrap/) | `liberado-common`, `liberado-config`, `liberado-cron`, `liberado-daemon`, `liberado-dispatcher`, `liberado-dispatch-pack`, `liberado-notify`, `liberado-orchestrator`, `liberado-executor`, `liberado-mcp`, `liberado-provider`, `liberado-provider-openai-compat` | Composition helpers that build Liberado's provider/dispatcher/orchestrator from the process environment — the shared daemon-assembly logic for every binary, so the env wiring lives in one place. |
| [`liberado-cli`](../../crates/cli/) | `liberado-server`, `chat-client-contract` | the `liberado` binary: a client + launcher — `serve` runs the daemon/API, `chat` is a streaming client |
| [`liberado-daemon`](../../crates/daemon/) | `liberado-common`, `liberado-notify`, `liberado-vault`, `liberado-dispatcher`, `liberado-orchestrator`, `liberado-session` | The Liberado daemon (Decision 2, daemon-first): the long-running core that watches the vault, attributes changes (loop-breaking), and emits reactable events. v1 vertical slice. |
| [`liberado-server`](../../crates/server/) | `chat-client-contract`, `liberado-bootstrap`, `liberado-chat-search`, `liberado-common`, `liberado-config`, `liberado-daemon`, `liberado-dispatcher`, `liberado-mcp`, `liberado-executor`, `liberado-main-agent`, `liberado-conversation-store`, `liberado-provider`, `liberado-telegram-approvals`, `liberado-memory-store`, `liberado-vault`, `liberado-session`, `liberado-session-store`, `liberado-coder-agent`, `liberado-notify` | The Liberado daemon's API server (library): the watch loop + chat + HTTP/SSE API. Runnable via `liberado serve`. |

## tooling

Meta tooling (evals, heuristics tuner). Not build dependencies of the system.

| Crate | Internal deps | Description |
|---|---|---|
| [`liberado-eval`](../../crates/eval/) | `liberado-common`, `liberado-config-loader`, `liberado-dispatcher`, `liberado-provider`, `liberado-provider-openai-compat` | The heuristic-tuning instrument (testing-and-eval-spec §4.2): runs the real dispatcher over a labeled set of routing scenarios and reports routing accuracy, safe-default rate, and the safety-regression gate — used to A/B the system prompt and tune the guards. |
| [`liberado-heuristics-tuner`](../../crates/heuristics-tuner/) | `liberado-coder-agent`, `liberado-coder-core`, `liberado-common`, `liberado-config`, `liberado-dispatcher`, `liberado-eval`, `liberado-executor`, `liberado-orchestrator`, `liberado-provider`, `liberado-provider-openai-compat` | Automates prompt-tuning for dispatcher, executor/subagent tool loops, and Liberado coder-role system prompts via beam search; proposes diffs + rubrics for human review. Never auto-applies prompt changes. |

## testing

Dev-dependency-only test support.

| Crate | Internal deps | Description |
|---|---|---|
| [`liberado-test-support`](../../crates/test-support/) | `liberado-common`, `liberado-executor`, `liberado-provider` | Shared ToolRuntime/RuntimeFactory test doubles, consolidating what used to be duplicated across liberado-orchestrator's and liberado-daemon's own test modules. Test-only: consumed exclusively as a dev-dependency. |
