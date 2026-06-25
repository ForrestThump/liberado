//! # liberado-tui — ratatui terminal client for Liberado
//!
//! A native terminal UI that attaches to a running [`liberado-server`] over the
//! **same shared HTTP/SSE contract** (`docs/interface.md`) as the web UI and the
//! `liberado chat` REPL. Embeds **no** agent logic — it is purely a renderer and an
//! input box.
//!
//! The TUI is the primary interactive surface for daily use and the proof that the
//! contract is genuinely client-agnostic (Decision 2 daemon-first).
//!
//! ## Architecture
//!
//! ```text
//!   main.rs (tokio::main)
//!     │
//!     ├─► ratatui draw loop ──► ui.rs (read App, render frames)
//!     │
//!     ├─► crossterm input task ──► Action::Input(key)
//!     │
//!     ├─► HTTP poll task ──► api.rs ──► Action::StatusUpdate / ReactionsUpdate
//!     │
//!     └─► SSE stream task ──► sse.rs ──► Action::Token / Tool / Done / Failed
//!                │
//!        app.rs (App state machine)
//!          App::update(Action) → Vec<Effect>
//! ```
//!
//! All state lives in `App` behind a single lock. The draw loop reads it immutably;
//! background tasks send `Action`s that flow through `App::update()`, which returns
//! `Effect` instructions for `main` to execute (spawn HTTP, quit, etc.).

pub mod api;
pub mod app;
pub mod sse;
pub mod ui;

pub use api::{DaemonStatus, ReactionEvent, ConvHeader, ChatMessage, ToolCallChip, ToolResultChip};
pub use app::{App, Action, Effect, Focus, Message};
