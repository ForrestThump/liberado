# liberado-tui — ratatui terminal client

A native terminal UI for Liberado that attaches to a running daemon server over the
**same shared HTTP/SSE contract** as the web UI and the `liberado chat` REPL
(`docs/spec/reference/api.md`). It is the primary interactive surface for daily use.

## Principle: the TUI proves the API is client-agnostic

The TUI is deliberately a **thin client** — it embeds no agent logic, no `ChatSessions`,
no provider, no store. Every byte it renders arrives over HTTP/SSE. If it can't express
its needs through the existing API, that's a gap in the contract, not the TUI. Running it
alongside the web UI against the same daemon is the proof that Liberado is genuinely
daemon-first (Decision 2).

**Loose coupling for WebUI reuse:** HTTP/SSE clients, session view-models, and pure
Action→state reducers must live in **shared crates** (`chat-client-contract` and/or a future
`liberado-client-core`). This crate owns **ratatui paint + terminal I/O only**. Do not grow
goal/chat business logic only in `tui` — WebUI must import the same brain. See
[`docs/future-work/tui-maturity-roadmap.md`](../../docs/future-work/tui-maturity-roadmap.md) §1.1.

## Views

Default layout is **sparse** (no permanent sidebar). Full-screen overlays for session/model pickers.

```
┌─ status ──────────────────────────────────────────────────┐
│  ● running · model · vault path                           │
├─ chat ────────────────────────────────────────────────────┤
│  user / assistant / tool chips                            │
├─ input ───────────────────────────────────────────────────┤
│  > _   (/ opens slash palette + ghost complete)           │
└───────────────────────────────────────────────────────────┘
```

| Overlay | Opened by | Behavior |
|---------|-----------|----------|
| Session browser | `/session` | Type-to-filter conversations; Enter reopens history |
| Model browser | `/model` | `GET /api/models`; Enter → `POST /api/models/select` |

### Chat pane
- SSE: `token` / `tool` / `tool_result`; markdown for assistant text.

### Status bar
- `/api/status` (connection, **live** model name after hot-swap, vault path).

### Input
- Slash catalog (`liberado-commands`). `/theme set <name>` persists to platform
  `liberado/settings.toml` (via `liberado-theme`).

## How data flows

```
liberado-tui ──HTTP──▶ liberado-server (:4201)
     │                       │
     │  POST /api/chat/stream │ (SSE: session → token* → [tool → tool_result]* → done|failed)
     │◀──────────────────────◁│
     │
     │  GET /api/status ──────▶ { running, model_name, ... }
     │  GET /api/models ──────▶ { models, current }
     │  POST /api/models/select ▶ hot-swap
     │  GET /api/conversations ──────▶ [ { id, title, created_at }, ... ]
     │◀───────────────────────────────
```

All endpoints share one `reqwest::Client`. The incremental SSE byte-stream decoder
(`SseDecoder`/`SseEvent`) itself lives in `chat_client_contract::native`, shared with the
`liberado chat` CLI client (which used to carry its own separate copy) — see
`docs/future-work/archive/tui-shared-code-extraction-plan.md`. This crate's own `src/sse.rs` only converts a
decoded `SseEvent` into this crate's `Action` enum (a `ToAction` trait, since Rust's orphan rules
don't allow an inherent `impl` on a foreign type).

## State model

The TUI holds a single `App` struct behind an `Arc<Mutex<App>>` (or similar) that
ratatui's draw closure reads:

```
App {
    server: "http://127.0.0.1:4201",

    // Chat
    session: Option<Ulid>,      // conversation id (learned from `session` event)
    messages: Vec<Message>,     // rendered messages (user + assistant + tool chips)
    input: String,              // composer buffer
    streaming: bool,            // true while a turn is in-flight (SSE stream open)

    // Sidebar
    status: DaemonStatus,       // cached from GET /api/status
    reactions: Vec<ReactionEvent>, // tail from GET /api/reactions
    conversations: Vec<ConvHeader>, // sidebar list

    // Focus
    focus: Focus,               // Input | SidebarConversations
    scroll_offset: usize,       // chat scrollback
}
```

Events (user input, SSE events, HTTP responses, ticker) are fed into `App` via an
`Action` enum handled by `App::update(action) -> Vec<Effect>`. `Effect` is an
instruction for `main` to execute (spawn an HTTP request, start an SSE stream, quit).

## Keyboard shortcuts

| Key | Action |
|-----|--------|
| `Enter` | Send message (when input focused) / Resume conversation (when sidebar focused) |
| `Shift+Enter` | Newline in composer |
| `Esc` | Cancel current turn / clear composer / return focus to input |
| `Ctrl+C` | Quit |
| `Tab` / `S-Tab` | Cycle focus: Input ↔ Sidebar |
| `j` / `k` / `↑` / `↓` | Navigate sidebar list / scroll chat |
| `n` | New conversation |
| `PgUp` / `PgDn` | Page-scroll chat |

## Modules

Substantially more decomposed than a first pass — `render/` and `handlers/` each split into one file
per pane/input-mode rather than one monolithic `ui.rs`/input-handling function.

| File | Role |
|------|------|
| `main.rs` | Binary entry: init terminal, spawn HTTP + SSE + input tasks, run ratatui draw loop |
| `lib.rs` | Crate docs + re-exports |
| `sse.rs` | Converts a `chat_client_contract::native::SseEvent` into this crate's `Action` (see above) |
| `api.rs` | Typed `reqwest` client: `post_chat_stream()`, `fetch_status()`, `fetch_reactions()`, `fetch_conversations()`. Structs for every API response shape |
| `app.rs` | `App` state machine: `Action` enum, `App::update(action)`. Pure state transitions — no I/O |
| `command_context.rs` | Implements `liberado_commands::CommandContext` for this TUI's `App`, so the shared slash-command dispatcher (`/help`, `/theme`, ...) can run against it |
| `conversations.rs` | Pure functions building/flattening a conversation tree from `ConvHeader` data for the sidebar list |
| `effects.rs` | `EffectRunner`: owns the shared state needed to actually execute the `Effect` instructions `App::update` produces (HTTP calls, SSE streams, terminal actions) |
| `format.rs` | Pure formatting utilities (timestamps, previews) shared by `app.rs`/`render/` |
| `handlers/` | Keyboard/mouse input handlers, one file per concern (`chat.rs`, `dialog.rs`, `input.rs`, `mouse.rs`, `sidebar.rs`) — each a free `handle(app, key) -> Vec<Effect>`, dispatched by focus |
| `render/` | Rendering, one file per pane (`chat.rs`, `dialog.rs`, `input.rs`, `sidebar_conversations.rs`, `sidebar_reactions.rs`, `sidebar_status.rs`, `status_bar.rs`) behind a `draw()` entry point that lays out the frame and dispatches to each. Pure rendering — reads `App`, never mutates |
| `terminal.rs` | `TerminalGuard`: raw mode, alternate screen, mouse capture lifecycle |
| `tuning.rs` | Tunable constants (scrollback limits, poll intervals, etc.) kept in one place |

## Dependencies

- `ratatui` + `crossterm` — terminal rendering and raw-mode input.
- `reqwest` — HTTP client (the same one `liberado chat` uses).
- `tokio` — async runtime, channels for event dispatch.
- `serde_json` — parse API responses and SSE tool events.
- `tracing` — structured logs (stderr, so the TUI never garbles them).
- `chat_client_contract` — the shared wire types, `SseDecoder`/`SseEvent`.
- `liberado_commands` — the shared slash-command dispatcher (`command_context.rs` implements its
  `CommandContext` trait for this crate's `App`).
- `liberado_markdown` — the shared Markdown-to-terminal-lines parser.
- `liberado_theme` — the shared color-token `Theme`/`ThemeRegistry`.

## Maturity status (2026-07-10)

This TUI is a **strong chat + daemon client**, not yet peer-class with Claude Code / Grok Build /
OpenCode for **agent/goal UX**. Backend goal sessions exist (`POST /api/goals`, SSE); the TUI does
not consume them yet.

**Living roadmap:** [`docs/future-work/tui-maturity-roadmap.md`](../../docs/future-work/tui-maturity-roadmap.md)

### Landed (engineering)

- Markdown via `liberado-markdown`; themes via `liberado-theme`
- Esc cancel + Ctrl+S stop streaming; mouse focus/scroll
- Conversation list/filter/tree stubs; slash commands; status + reactions sidebar
- Production hardening (timeouts, backpressure, message cap, SIGTERM)

### Critical gaps (product)

- **Goal session mode** (start/stream/cancel life + coding packs)
- **Intake / freeze / verifier** visibility
- **Coding density** (diff, file list, budget/turn HUD, collapsible tool timeline)
- **Performance** (cache markdown lines; dirty redraw; virtualize chat)
- **Command palette** and multi-session agent HUD
