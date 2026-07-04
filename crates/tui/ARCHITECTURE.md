# liberado-tui — ratatui terminal client

A native terminal UI for Liberado that attaches to a running daemon server over the
**same shared HTTP/SSE contract** as the web UI and the `liberado chat` REPL
(`docs/reference/api.md`). It is the primary interactive surface for daily use.

## Principle: the TUI proves the API is client-agnostic

The TUI is deliberately a **thin client** — it embeds no agent logic, no `ChatSessions`,
no provider, no store. Every byte it renders arrives over HTTP/SSE. If it can't express
its needs through the existing API, that's a gap in the contract, not the TUI. Running it
alongside the web UI against the same daemon is the proof that Liberado is genuinely
daemon-first (Decision 2).

## Views

The terminal has a fixed layout; the user's focus shifts between areas, not pages:

```
┌─ Chat ────────────────────────────┬─ Sidebar ─────────────┐
│                                   │  Daemon: ● running    │
│  > What's on my calendar today?   │  Uptime: 3h 12m       │
│                                   │  Vault: /notes        │
│  You have two events:             │                       │
│   • 10:00 — Standup               │  Recent reactions:    │
│   • 14:00 — Dentist               │  → observed inbox/x   │
│                                   │  → decided:clarify    │
│                                   │  → acted:reported     │
│                                   │                       │
├───────────────────────────────────┤  [1] conv-1 about...  │
│  > _                              │  [2] conv-2 review... │
└───────────────────────────────────┴───────────────────────┘
```

### Chat pane (left, 70%)
- Renders the live SSE stream: `token` deltas build the assistant message inline;
  `tool` / `tool_result` pairs appear as inline status chips.
- Scrollback history for the current conversation, loaded on resume.
- Markdown rendering (code blocks, lists, links).

### Status bar (top of sidebar)
- Daemon liveness (`/api/status`): running/stopped, uptime, watcher active,
  dispatcher/orchestrator attached.
- Vault path + note count (`/api/vault`).

### Reactions feed (sidebar, below status)
- Tail of recent `ReactionEvent`s from `/api/reactions?limit=N`.
- One line per reaction: icon by outcome (observed/decided/acted), path, correlation id.

### Conversation list (sidebar bottom)
- `GET /api/conversations` — list of conversation headers, newest first.
- Navigate with arrow keys; Enter to resume; `n` to start a new conversation.
- Resume rehydrates the conversation from the store and renders prior messages
  before accepting input.

### Input line (bottom, full width)
- Multi-line composer (Enter sends, Shift+Enter for newline, Esc to cancel).
- Session id is carried in the SSE stream — the user never sees it.

## How data flows

```
liberado-tui ──HTTP──▶ liberado-server (:4201)
     │                       │
     │  POST /api/chat/stream │ (SSE: session → token* → [tool → tool_result]* → done|failed)
     │◀──────────────────────◁│
     │
     │  GET /api/status ──────▶ { running, uptime, dispatcher_attached, ... }
     │◀────────────────────────
     │
     │  GET /api/reactions?limit=20 ▶ [ { event_type, timestamp, path, outcome }, ... ]
     │◀────────────────────────────────
     │
     │  GET /api/conversations ──────▶ [ { id, title, created_at }, ... ]
     │◀───────────────────────────────
```

All endpoints share one `reqwest::Client`. The incremental SSE byte-stream decoder
(`SseDecoder`/`SseEvent`) itself lives in `chat_client_contract::native`, shared with the
`liberado chat` CLI client (which used to carry its own separate copy) — see
`docs/roadmap/tui-shared-code-extraction-plan.md`. This crate's own `src/sse.rs` only converts a
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

## Done since this doc's first pass (previously listed here as deferred)

- **Markdown-to-terminal rendering** — `render/chat.rs` calls `liberado_markdown::markdown_to_lines()`
  on every assistant message; the parser itself lives in the shared `liberado-markdown` crate (used by
  ratatui, Dioxus, and terminal output alike), not a TUI-local one.
- **Color themes** — `liberado-theme`'s `ThemeRegistry` is a dependency; themes are discovered from
  `<config>/themes/*.toml` and selectable via `command_context.rs`'s `/theme` command support.

## What's still deferred

- Conversation branching / DAG views — the session sidebar lists linear conversations
  only.
- A "stop" keybinding that closes the SSE stream mid-turn (Esc already does this
  implicitly by dropping the reqwest response; the backend cancel-on-disconnect
  primitive is already built).
