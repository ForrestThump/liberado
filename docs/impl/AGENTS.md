# Liberado — Agent & Build Guide

This file is the single source of truth for agents and contributors on how to build, run, and
extend Liberado. Read [`../spec/architecture/overview.md`](../spec/architecture/overview.md) for the system
overview and current status, and
[`development-workflow.md`](development-workflow.md) for *how work here actually gets done* —
research/plan/implement/test/document/commit discipline, subagent delegation patterns, and the
project's governing principles. Read that one before starting non-trivial work on a new branch.

---

## Workspace layout

```
crates/
  common/               # Shared types — compile first
  config/               # Config-file loader/validator (Decision 14), dependency-light
  config-loader/        # ConfigSource trait + ChainLoader beneath `config`
  provider/             # Provider trait + MockProvider
  provider-openai-compat/ # One config-driven Provider for any OpenAI-compatible API (DeepSeek,
                          # OpenRouter, ...) — replaces what used to be one crate per backend
  vault/                # Turbovault adapter (attribution, loop-breaking)
  dispatcher/           # Goal ? DispatchDecision (LLM classify + deterministic guards)
  executor/             # Bounded tool-calling agent loop ? Report
  mcp/                  # TurbomcpRuntime (ToolRuntime over real MCP stdio/SSE)
  orchestrator/         # Bridges DispatchDecision ? execution
  main-agent/           # Multi-turn Conversation (chat loop): context + streaming + tool use
  conversation-store/   # Decision-17 append-only JSONL conversation store
  daemon/               # Watch ? debounce ? attribute ? dispatch (the long-running core)
  bootstrap/            # Shared env?provider/dispatcher/orchestrator wiring for the binaries
  chat-client-contract/ # Shared wire types + SseDecoder for all chat clients
  liberado-commands/    # Shared slash-command parser/handlers for all chat clients
  markdown/             # Shared, UI-agnostic Markdown parser
  theme/                # Shared color-token Theme/ThemeRegistry
  tui/                  # ratatui TUI client (same chat/SSE contract as web UI/CLI)
  cli/                  # liberado binary — client + launcher (serve runs the daemon, chat streams)
  webui/                # Dioxus WASM frontend  (excluded from native workspace build)
  server/               # the daemon's API server library — watch + chat + HTTP/SSE; run via `liberado serve`
  eval/                 # Real-model routing/safety eval suite (not a build dependency)
  heuristics-tuner/     # Automated dispatcher/executor/subagent prompt-tuning tool (not a build dependency)
  mcp-forge/            # Builds/installs Liberado MCP servers from git URLs
  test-support/         # Dev-dependency-only shared ToolRuntime/RuntimeFactory test doubles

  # Coder pack (roadmap Priority 3)
  coder-core/           # Provider-agnostic coder contracts: tasks, backend trait, sandbox specs, events, reports, traces
  coder-agent/          # Coding domain pack: goal-session backend over Executor + coding tools
  coder-runner/         # Process boundary for the coder: the liberado-coder-run subprocess bridge
  coder-sandbox/        # Workspace and command sandbox abstractions for the Rust-native coder
  coder-tools/          # Executor ToolRuntime implementation for the Rust-native coder

  # Session kernel
  session/              # Session kernel (D7): GoalSpec, GoalSessionHub, SessionEvent, DomainPackRunner
  session-store/        # Converged session store (D7): one JSONL log; ConversationStore + SessionRecordStore lenses

  # Infrastructure
  cron/                 # Cron as an EventSource (D18/19) — schedule fires like vault-watch, vault-agnostic
  messaging/            # Channel-agnostic messaging traits for chat clients (Telegram today; Matrix/Signal/Discord later)
  notify/               # Pluggable notification sink for events a human should know about (Telegram first)
  scratchpad/           # Per-execution scratchpad (todo-list) tool — engine-injected, not an MCP
  dispatch-pack/        # Domain pack running dispatcher + orchestrator as a GoalSessionHub pack (E2 convergence)
  chat-search/          # Full-text/regex search over session history (shared by REST endpoint + MCP tool)
  chat-search-mcp/      # MCP server exposing conversation-history search as a tool (stdio)
  memory-mcp/           # MCP server exposing general + procedural memory as tools, backed by memory-store
  memory-store/         # Vault-backed general/procedural memory stores with turbovault-vector semantic recall
  telegram-approvals/   # Approval/chat bot: Approve/Reject/Revise taps ? proposal frontmatter edits

turbovault/          # Co-developed path dep (sibling repo, not a workspace member)
turbomcp/            # Co-developed path dep (sibling repo, not a workspace member)
```

Full generated inventory: [`docs/spec/reference/crate-map.md`](../spec/reference/crate-map.md) — regenerate with `just gen-crate-map`.

---

## Prerequisites

```
Rust stable (1.90+) via rustup   — https://rustup.rs
wasm32 target (for webui only)   — rustup target add wasm32-unknown-unknown
dioxus-cli (for webui only)      — cargo install dioxus-cli --locked
```

---

## Building the Rust workspace (all native crates)

```cmd
cargo build
cargo test --workspace
```

`crates/webui` is a workspace member but is a WASM-only binary; on a native build the main
function emits a message and exits. All other crates compile normally.

---

## Running the daemon (watch-only / with dispatcher)

```cmd
rem Watch-only (no DEEPSEEK_API_KEY):
cargo run --bin liberado -- <vault-path>

rem With dispatcher (set DEEPSEEK_API_KEY):
set DEEPSEEK_API_KEY=sk-...
cargo run --bin liberado -- <vault-path>

rem With dispatcher + MCP execution: declare MCPs in topology.toml ([[mcps]] with a transport).
rem Both the dispatcher catalog and the runtime connection derive from topology.mcps (single source).
set DEEPSEEK_API_KEY=sk-...
cargo run --bin liberado -- <vault-path>
```

Reactions are logged to stderr. The daemon runs until killed.

---

## CLI chat client

`liberado chat [session-id]` is a terminal REPL for the conversational agent. It is a **client** of
the shared chat API — a thin `reqwest`/SSE consumer of `POST /api/chat/stream` (`docs/spec/reference/api.md`),
the first native (non-browser) client of that contract. It embeds no agent logic; the daemon server
owns the conversation, provider, and store.

It therefore needs the **server running first** in another terminal:

```cmd
rem Terminal 1 — the server (needs DEEPSEEK_API_KEY for chat):
set DEEPSEEK_API_KEY=sk-...
cargo run --release --bin liberado -- serve <vault-path>

rem Terminal 2 — the chat client:
cargo run --bin liberado -- chat

rem Resume an existing conversation by id:
cargo run --bin liberado -- chat <session-id>

rem Point at a non-default server:
set LIBERADO_SERVER=http://192.168.1.10:4201
cargo run --bin liberado -- chat
```

Type a message at the `> ` prompt; the answer streams back inline. Tool calls print as
`[tool] name(args)` / `[tool] name ok|err preview`. Type `exit`/`quit` or Ctrl-D to leave. The first
turn starts a new conversation (its id is learned from the stream and reused for the session);
passing a `session-id` continues a prior one.

---

## Configuration

Liberado loads its config (Decision 14) from a directory of small, optional TOML files. The
directory is resolved as `LIBERADO_CONFIG_DIR` if set, otherwise the platform config dir with a
`liberado/` subfolder (`%APPDATA%\liberado\` on Windows, `~/.config/liberado/` on Linux). Three
files are read, **each optional** — an absent file leaves that section at its built-in defaults:

| File            | Section  | What it holds                                                       |
|-----------------|----------|--------------------------------------------------------------------|
| `topology.toml` | wiring   | `vault_path`, provider/models, enabled MCP/hook components         |
| `policy.toml`   | security | zones + write-classes and the capability grants                    |
| `tuning.toml`   | behavior | dispatch/context/concurrency/capture/maintenance knobs (all defaulted) |

`policy.toml` is the **security surface**: it declares per-zone write classes (`agent_writable` /
`shared` / `human_only` / `proposal_only`; an unlisted zone fails safe to `proposal_only`) and the
capability grants. The daemon's dispatcher now runs with the **union of those grants** as its base
authority (Decision 4 — narrowed per dispatch, never widened); previously it ran with an *empty*
capability set. So a grant in `policy.toml` is what lets the agent act at all.

Each `[[mcps]]` entry in `topology.toml` declares three **required** fields beyond `name`:
`description` (the routing blurb the dispatcher matches a goal against), `consequence` (our risk
rating — one of `read_only` / `reversible` / `irreversible` / `external`; the consequence guard
gates direct action on it), and `transport` (how the runtime reaches the server). The first two are
required and have no default: Liberado owns risk classification (MCPs don't declare their own), and
`Consequence`'s default is the *unsafe* `read_only`, so an entry missing either is a load error
rather than a silent mis-rating. `transport` is an inline table, one of:

- `{ kind = "stdio", command = "npx", args = ["-y", "@scope/server"] }` — spawn a child process and
  speak MCP over its stdin/stdout (`args` defaults to empty).
- `{ kind = "http", url = "https://mcp.example.com/mcp" }` — connect to a remote MCP endpoint.

`topology.mcps` is the **single source** for both the dispatcher's catalog (the enabled MCPs it may
route to) and the runtime's actual connection (each enabled MCP is connected by `name` via its
`transport`), so every name the dispatcher routes to is a name the runtime can reach.

Validate a config without starting the daemon:

```cmd
cargo run --bin liberado -- config check
```

This loads + validates the resolved config and prints a summary (config dir, vault path, #
zones/grants/MCPs), or the first actionable error (e.g. a grant referencing an undeclared zone, an
`ExecuteMcp` naming an MCP absent from `topology.mcps`, or a `secret_refs` entry with no env var).
With no config present it correctly reports the missing `vault_path`.

Commented starter files live in [`config.example/`](../../config.example/) — copy them to your
config dir and edit.

---

## Web UI

### Running the server (daemon + HTTP API + static frontend)

```cmd
rem From the workspace root:
cargo run --release --bin liberado -- serve <vault-path>

rem Or with env var:
set LIBERADO_VAULT=C:\path\to\vault
cargo run --release --bin liberado -- serve
```

The server listens on `0.0.0.0:4201` (LAN-accessible). Set `LIBERADO_PORT` to change.

**Environment variables:**

| Variable            | Default | Description                                    |
|---------------------|---------|------------------------------------------------|
| `LIBERADO_VAULT`    | —       | Path to the Obsidian vault (required)          |
| `LIBERADO_PORT`     | `4201`  | Server listen port                             |
| `LIBERADO_DATA_DIR` | `.liberado` | Operational data root: conversations, dispatch journals |
| `LIBERADO_CONFIG_DIR` | platform `liberado/` | Daemon policy/topology/tuning TOML (optional) |
| `DEEPSEEK_API_KEY`  | —       | Enables provider (DeepSeek); see also OpenRouter profiles |
| `DEEPSEEK_MODEL`    | `deepseek-chat` | Boot default model (hot-swappable after start via API/TUI) |

**API endpoints** (full contract: [`../spec/reference/api.md`](../spec/reference/api.md)):

| Endpoint                    | Description                                      |
|-----------------------------|--------------------------------------------------|
| `GET /api/status`           | Daemon state; `model_name` tracks hot-swap       |
| `GET /api/models`           | Live provider model catalog + `current`          |
| `POST /api/models/select`   | Hot-swap active model — `{"model":"…"}` (no restart) |
| `GET /api/catalog`          | Live MCP catalog + main-agent/dispatcher visibility |
| `GET /api/reactions?limit=N`| Recent reaction events (default 20)              |
| `GET /api/vault`            | Vault root path and watcher info                 |
| `POST /api/chat`            | Face-agent chat (delegate ? mesh) when `delegation_mode` |
| `GET`/`POST /api/chat/stream` | Streaming chat SSE contract                    |
| `GET /api/conversations`    | List conversation headers                        |
| `GET /api/conversations/{id}` | Full message history                           |
| `PATCH /api/conversations/{id}` | Rename                                       |
| `GET /`                     | Dioxus WASM frontend (served from dist/)         |

**On-disk ops data** (outside vault):

| Path | Contents |
|------|----------|
| `<LIBERADO_DATA_DIR>/conversations/*.jsonl` | Face chat sessions |
| `<LIBERADO_DATA_DIR>/dispatches/chat-delegate-*.jsonl` | Mesh delegation journals (classify + disposition) |
| Platform `liberado/settings.toml` | TUI theme preference (`theme = "nord"`) |
| Platform `liberado/themes/*.toml` | Optional custom theme files |

**TUI dogfood:**

```cmd
cargo build -p liberado
.\scripts\start-dev-stack.ps1 -Restart
cargo run -p liberado-tui
```

Slash: `/session` (session browser), `/model` (model list + Enter to hot-swap), `/theme set <name>`
(persists). After code changes that touch the daemon, rebuild and restart the stack — a stale
`liberado.exe` is a common source of “fixed but still broken” dogfood.

**Policy tip:** grant vault tools on `component = "dispatcher"` (e.g. `ExecuteMcp = "turbovault"`),
not on `main-agent`, when using face-agent mode.

### Building the WASM frontend

**Windows note:** There are two Rust installations on this machine. The standalone install at
`C:\Program Files\Rust stable MSVC 1.94\` is first in PATH but lacks the wasm32 stdlib.
The rustup-managed toolchain at `C:\Users\Shiloh\.rustup\toolchains\stable-x86_64-pc-windows-msvc\`
has it. You must prepend the rustup bin to PATH before calling `dx`:

```cmd
rem Build (release):
set "PATH=C:\Users\Shiloh\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin;%PATH%"
set "CARGO=C:\Users\Shiloh\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\cargo.exe"
dx build --release --package liberado-webui --web

rem The built frontend lands at:
rem   target/dx/liberado-webui/release/web/public/
rem The server serves from that directory automatically.
```

After building the frontend, start the server — it serves both the API and the static WASM bundle
on one port. Open `http://<your-lan-ip>:4201/` in any browser on the LAN.

---

## Running tests

```cmd
rem All workspace crates:
cargo test --workspace

rem Single crate:
cargo test -p liberado-daemon
cargo test -p liberado-dispatcher
```

The dispatcher and executor tests use `MockProvider` from `crates/provider` — no API key needed.

---

## Co-development with Turbovault / Turbomcp

`turbovault/` and `turbomcp/` are sibling repos (path deps, not workspace members). If they are
not checked out next to this repo, `cargo build` will fail. Clone them:

```cmd
cd ..
git clone https://github.com/shilohmangus/turbovault
git clone https://github.com/shilohmangus/turbomcp
cd life-os
cargo build
```

A root `[patch.crates-io]` in `Cargo.toml` redirects the published `turbomcp` to the local fork
so the whole tree builds against one turbomcp (the one carrying `_meta` pass-through needed for
provenance loop-breaking).

---

## Key environment variables (all crates)

| Variable            | Used by              | Effect                                                   |
|---------------------|----------------------|----------------------------------------------------------|
| `LIBERADO_VAULT`    | cli, server          | Path to the Obsidian vault                               |
| `LIBERADO_SERVER`   | cli (`chat`)         | Daemon server base URL for the chat client (default `http://127.0.0.1:4201`) |
| `DEEPSEEK_API_KEY`  | server               | Enables the dispatcher (DeepSeek inference)              |
| `LIBERADO_PORT`     | server               | Override listen port (default 4201)                      |
| `LIBERADO_DATA_DIR` | server               | Operational-data root (default `.liberado`); conversation logs live under `<dir>/conversations` |
| `RUST_LOG`          | all                  | Log level (e.g. `info`, `debug`, `liberado_daemon=trace`)|

---

## Configuration (Decision 14)

Config is a **mesh**: each file (`topology.toml`, `policy.toml`, `tuning.toml`) is independently resolved from four overlay layers (bottom ? top):

1. Built-in `Default`
2. `LIBERADO_CONFIG_DIR/<file>` (if the env var is set and the file exists)
3. Root `config/<file>` (runtime override)
4. `<crate>/config/<file>` (compile-time examples only via `CARGO_MANIFEST_DIR`; not runtime unless explicitly passed a path)

Later layers win at the TOML table/key level. Per-value provenance is reported by `liberado config check`.

See [`../spec/config-spec.md`](../spec/config-spec.md) for the full rationale,
file layout, and validation contract.
