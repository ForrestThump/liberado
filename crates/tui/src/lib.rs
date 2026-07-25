//! # liberado-tui — ratatui terminal client for Liberado
//!
//! A native terminal UI that attaches to a running daemon server over the
//! **same shared HTTP/SSE contract** (`docs/reference/api.md`) as the web UI and the
//! `liberado chat` REPL. Embeds **no** agent logic — it is purely a renderer and an
//! input box.
//!
//! The TUI is the primary interactive surface for daily use and the proof that the
//! contract is genuinely client-agnostic (Decision 2 daemon-first).
//!
//!
//! # Architecture
//!
//! ```text
//! main.rs (tokio::main)
//!   │
//!   ├──► TerminalGuard::enter() ──► raw mode + alternate screen + mouse capture
//!   │
//!   ├──► spawn_poller() ──► api.rs ──► Action::{StatusUpdate, ReactionsUpdate, ConversationsUpdate}
//!   │                                                                                  │
//!   ├──► crossterm input ──► App::handle_key()  ──┐                                   │
//!   │         │                                     │                                   │
//!   │         ├──► handlers/input.rs                ├──► Vec<Effect> ──► EffectRunner  │
//!   │         ├──► handlers/sidebar.rs              │        │                         │
//!   │         └──► handlers/chat.rs                 │   tokio::spawn(                  │
//!   │                                               │     reqwest → SSE → Action)     │
//!   ├──► crossterm mouse ──► handlers/mouse.rs ──────┤        │                         │
//!   │                                               ├──► Vec<Effect> ──► EffectRunner  │
//!   │          ┌── Action::SseToken                 │        │                         │
//!   │          ├── Action::SseTool                  │   tokio::spawn(                  │
//!   │   sse.rs ├── Action::SseToolResult  ──────────┤     reqwest → SSE → Action)     │
//!   │          ├── Action::SseDone                  │        │                         │
//!   │          └── Action::SseFailed                │   api.rs                         │
//!   │                                               │        │                         │
//!   │    App::update(Action) ──► Vec<Effect> ───────┘   HTTP responses ◄──────────────┘
//!   │
//!   ├──► ratatui draw loop ──► ui.rs → render/ (chat, sidebar, status_bar, input)
//!   │
//!   └──► TerminalGuard drop ──► restore terminal on exit/panic
//! ```
//!
//! All state lives in [`App`] behind a single `Arc<Mutex<>>`. The draw loop reads it
//! immutably; background tasks send [`Action`]s through a bounded `mpsc::channel`
//! (capacity `tuning::ACTION_CHANNEL_CAPACITY`); `App::update()` processes each action
//! and returns [`Effect`] instructions that [`EffectRunner`] executes (spawn HTTP, cancel
//! SSE, quit, etc.).
//!
//!
//! # Module map
//!
//! | Module | Responsibility | Key public types |
//! |--------|---------------|-----------------|
//! | `app` | State machine, Action→Effect reducer, stream/scroll/tree utilities. | `App`, `Action`, `Effect`, `Message`, `Focus`, `StatusSummary`, `VisibleNode`, `LayoutRects` |
//! | `api` | Typed HTTP client. Serde structs for every endpoint response. | `DaemonStatus`, `ConvHeader`, `ReactionEvent` |
//! | `command_context` | `impl CommandContext for App` — wires the shared `liberado-commands` slash-command crate (`/new`, `/help`, `/theme`, `/session`, etc.) to TUI state. | `CommandContext for App` |
//! | `conversations` | Conversation tree builder and flattener. | `visible_tree()`, `filtered_list()` |
//! | `effects` | Side-effect execution runtime. Spawns SSE/HTTP tokio tasks. | `EffectRunner`, `StreamState` |
//! | `format` | Shared formatting utilities (no App dependency). | `relative_time`, `truncate_for_display`, `short_id`, `truncate_path` (`format_uptime` re-exported from `liberado-commands` instead — shared with the other clients) |
//! | `handlers` | Keyboard/mouse input handlers, one module per focus/device. | `input::handle`, `sidebar::handle`, `chat::handle`, `mouse::handle`, `point_in_rect` |
//! | `render` | Ratatui rendering — one module per pane. | `draw()`, `chat::draw`, `sidebar_status::draw`, `sidebar_reactions::draw`, `sidebar_conversations::draw`, `status_bar::draw`, `input::draw` |
//! | `sse` | TUI-specific `SseEvent -> Action` conversion. The decoder itself (`SseDecoder`/`SseEvent`) lives in `chat_client_contract::native`, shared with the CLI. | `ToAction::to_action() -> Result<Action, String>` |
//! | `terminal` | RAII terminal lifecycle guard. | `TerminalGuard` |
//! | `tuning` | Compile-time constants for scroll, layout, timing, truncation. | `POLL_INTERVAL`, `MOUSE_SCROLL_LINES`, `INPUT_MIN_HEIGHT`, etc. |
//! | `ui` | Public `draw()` entry point; `c()` color resolver. | `draw()`, `c()` |
//!
//!
//! # Public API — types re-exported from `lib.rs`
//!
//! These are the types an integrating agent needs to wire the TUI into a main loop.
//!
//! ## Core state machine
//!
//! **`App`** — The single source of truth. Create with `App::new(server_url, theme_registry)`.
//! Drive it via three entry points:
//!
//! * `handle_key(key) → Vec<Effect>` — keyboard input (crossterm `KeyEvent`)
//! * `handle_mouse(event) → Vec<Effect>` — mouse clicks/scroll (crossterm `MouseEvent`)
//! * `update(action) → Vec<Effect>` — async events from the poller or SSE stream
//!
//! **`Action`** — Events pushed into `App::update()`. Variants:
//!
//! | Variant | Source | Description |
//! |---------|--------|-------------|
//! | `StatusUpdate(DaemonStatus)` | Poller | Daemon health + model/token info |
//! | `ReactionsUpdate(Vec<ReactionEvent>)` | Poller | Recent file-change reactions |
//! | `ConversationsUpdate(Vec<ConvHeader>)` | Poller / Refresh | Sidebar conversation list |
//! | `HistoryLoaded { id, messages }` | HTTP response | Full message history for a conversation |
//! | `SseSession(String)` | SSE stream | Conversation session id (first event) |
//! | `SseToken(String)` | SSE stream | Streaming text delta |
//! | `SseTool { name, args }` | SSE stream | Tool call started |
//! | `SseToolResult { name, ok, preview }` | SSE stream | Tool call completed |
//! | `SseDone` | SSE stream | Turn finished successfully |
//! | `SseFailed(String)` | SSE stream / HTTP | Error during streaming or history load |
//! | `ConnectionStatus(bool)` | Poller | Daemon connectivity transition |
//! | `Tick` | Poller | Periodic heartbeat (currently a no-op in `update()`) |
//!
//! **`Effect`** — Commands returned by `App` to be executed by the main loop / `EffectRunner`:
//!
//! | Variant | Effect |
//! |---------|--------|
//! | `StartChatStream { message, session }` | `POST /api/chat/stream` → spawn SSE task |
//! | `RefreshConversations` | `GET /api/conversations` → `ConversationsUpdate` |
//! | `LoadConversationHistory(id)` | `GET /api/conversations/{id}` → `HistoryLoaded` |
//! | `CancelStream` | Abort the in-flight SSE task |
//! | `ForkConversation { parent_id, after_turn }` | `POST /api/sessions/{id}/fork` → `Action::Forked`; lands the user in the branch |
//! | `SetWindowTitle(String)` | Set the terminal window title |
//! | `Quit` | Set the `AtomicBool` quit flag (lock-free) |
//! | `None` | No side-effect needed |
//!
//! ## Display types
//!
//! **`Message`** — A single chat message in the scrollback buffer:
//!
//! * `User(String)` — user input
//! * `Assistant(String)` — model reply (rendered through markdown parser)
//! * `ToolCall(ToolCallChip)` — `[tool] name(args)` inline chip
//! * `ToolResult(ToolResultChip)` — `[tool] name ok|err preview` outcome chip
//! * `System(String)` — italic gray status/error messages
//!
//! **`Focus`** — Which panel has keyboard focus: `Input`, `SidebarConversations`, or `ChatMessages`.
//!
//! **`StatusSummary`** — Computed snapshot: `connected`, `uptime`, `model_name`,
//! `token_usage_total`, `context_window`, `session_id`, `message_count`, `streaming`.
//! Call `App::status_summary()` for the status bar.
//!
//! ## API response types
//!
//! **`DaemonStatus`** — `GET /api/status` response. Fields: `running`, `vault_path`,
//! `uptime_seconds`, `watcher_active`, `dispatcher_attached`, `orchestrator_attached`,
//! `reactions_seen`, plus optional `model_name`, `token_usage_total`, `context_window`
//! (all `#[serde(default)]` — backward-compatible when server adds them).
//!
//! **`ConvHeader`** — One entry from `GET /api/conversations`. Fields: `id`, `title`,
//! `created_at`, `parent_conversation: Option<String>`, `spawned_by: Option<String>`.
//! The `parent_conversation` field drives the sidebar DAG tree.
//!
//! **`ReactionEvent`** — One entry from `GET /api/reactions`. Fields: `event_type`,
//! `timestamp`, `source`, `correlation_id`, `path`, `outcome`.
//!
//! ## Formatting utilities
//!
//! Pure functions with no App dependency:
//!
//! * `relative_time(&str) → String` — ISO 8601 → "5m ago", "yesterday", "Jun 12"
//! * `format_uptime(u64) → String` — seconds → "1h 23m" or "0m 45s"
//! * `truncate_for_display(&str, usize) → String` — "hello world..." with ellipsis
//! * `short_id(&str) → &str` — first 8 chars of an ID
//!
//!
//! # How to wire the TUI (see `main.rs` for the canonical example)
//!
//! ```ignore
//! // 1. Enter terminal mode (restores on drop)
//! let (_guard, mut terminal) = TerminalGuard::enter()?;
//!
//! // 2. Build the app
//! let mut registry = ThemeRegistry::new();
//! let app = Arc::new(Mutex::new(App::new(server_url, registry)));
//!
//! // 3. Create the action channel
//! let (action_tx, mut action_rx) = mpsc::unbounded_channel();
//!
//! // 4. Start background poller (sends Action::StatusUpdate etc. on interval)
//! spawn_poller(action_tx.clone(), server_url, client.clone());
//!
//! // 5. Create the effect runner
//! let runner = EffectRunner { app, action_tx, client, stream_state };
//!
//! // 6. Run the event loop
//! loop {
//!     // Read keyboard/mouse → App::handle_key() / handle_mouse() → effects
//!     // Drain action channel → App::update() → effects
//!     // Execute effects via runner.run(effect)
//!     // Draw frame: ui::draw(frame, &mut app, spinner_tick)
//! }
//! ```
//!
//!
//! # Extension points
//!
//! * **New keyboard shortcut** — Add a handler in `handlers/input.rs`, `handlers/sidebar.rs`,
//!   or `handlers/chat.rs`, depending on the active focus. Each handler is a free function
//!   `pub(crate) fn handle(app: &mut App, key: KeyEvent) → Vec<Effect>`.
//! * **New slash commands** — Add it to the shared `liberado-commands` crate (a variant on
//!   `SlashCommand`, a case in `parse()`, a handler in `handlers/`, a route in `dispatch()`).
//!   `App::handle_slash_command()` in `app.rs` maps the resulting `CommandResult`s to `Effect`s;
//!   `command_context.rs` (`impl CommandContext for App`) is where state access is wired up.
//! * **New SSE event types** — Add a variant to `SessionEventKind` (`chat-client-contract`), a branch
//!   in `ToAction::to_action()` in `sse.rs`, a variant to `Action`, and a handler in `App::update()`.
//! * **New effects** — Add a variant to `Effect`, a branch in `EffectRunner::run()`,
//!   and return it from `App::update()` or `App::handle_key()`.
//! * **New daemon status fields** — Add to `DaemonStatus` with `#[serde(default)]`
//!   for backward compatibility, then read in `App::status_summary()` and display
//!   in `render/status_bar.rs` or the `/status` command.
//! * **New theme tokens** — Add to `Theme` struct in `liberado-theme`, set defaults
//!   in `default_dark()`/`default_light()`, update `layered_on()`, then reference
//!   via `c()` in `ui.rs`.
//!
//! # Decisions & invariants
//!
//! * The `App` struct is **not** `Clone` or `Copy` — all mutation goes through its
//!   methods, which return effects.
//! * `App::update()` never spawns async work; it only mutates state and returns
//!   `Vec<Effect>`. The caller (main loop / `EffectRunner`) executes effects.
//! * The `App` field `conversations` is the flat list from the API. The tree
//!   (sidebar DAG) is computed on demand by `conversations::visible_tree()`.
//! * An `Arc<AtomicBool>` quit flag is the only way to exit the event loop. `Effect::Quit`
//!   sets it (lock-free); the loop checks it each iteration.
//! * All colors route through `c()` in `ui.rs` / `render/`, which looks up theme tokens with
//!   hardcoded fallbacks. Switching themes is instant (no restart needed).
//! * Layout, timing, and display thresholds are compile-time constants in `tuning.rs`.
//!   Future: load from `tuning.toml` (alongside `topology.toml`/`policy.toml`) for
//!   no-recompile tweaking.
//! * Mutex is `parking_lot::Mutex`, not `std::sync::Mutex`. Poisoning is ignored —
//!   a panic in a handler will not crash the TUI.
//! * SSE streams have a 60-second timeout (`tuning::SSE_STREAM_TIMEOUT`). If no
//!   data arrives in that window, an `SseFailed` action is emitted.
//! * Message history is capped at `tuning::MAX_MESSAGE_COUNT` (500) on
//!   `HistoryLoaded`. A system marker replaces evicted messages.
//! * OS signals (SIGTERM, Ctrl+Break) are handled by the `ctrlc` crate, which
//!   sets the lock-free `AtomicBool` quit flag for a clean terminal restore via `TerminalGuard`.
//! * The mpsc action channel is bounded at `tuning::ACTION_CHANNEL_CAPACITY` (256). All
//!   sends use `try_send()` with `tracing::warn!` on a full channel.
//! * SSE JSON parse failures are surfaced as `Action::SseFailed` events in the chat pane,
//!   not silently swallowed.
//! * Mouse hit-testing uses explicit bounds checks via the `point_in_rect(col, row, rect)`
//!   helper, not `Rect::intersects` with a 1×1 rect hack.

pub mod api;
pub mod app;
pub mod command_context;
pub mod conversations;
pub mod effects;
pub mod format;
pub mod handlers;
pub mod md_cache;
pub mod render;
pub mod sse;
pub mod terminal;
pub mod tuning;
pub mod ui;

pub use api::{ConvHeader, DaemonStatus, ReactionEvent};
pub use app::{Action, App, Effect, Focus, Message, StatusSummary};
pub use format::{relative_time, short_id, truncate_for_display};
pub use liberado_commands::format_uptime;
