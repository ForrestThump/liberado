//! Shared HTTP/SSE wire contract for Liberado chat clients.
//!
//! Every client (TUI, CLI, WebUI) and the server depend only on this crate for
//! the types they exchange over the API.
//!
//! # Module layout
//!
//! - **`wire`** — All wire DTOs (`DaemonStatus`, `ReactionEvent`, `SessionEvent`, etc.).
//!   Pure `serde` — no native deps, compiles to `wasm32-unknown-unknown`. The clients'
//!   actual shared boundary lives here: [`wire::SessionEvent::from_sse_data`] turns one decoded
//!   SSE event into a typed [`wire::SessionEvent`] — the **converged** vocabulary (2026-07-11)
//!   for both the chat stream and the goal-session stream, so one decoder serves every surface.
//!   (The old chat-only `ChatEvent` was folded into it; a prior `ChatClient` trait in `native`
//!   promised a fuller shared client abstraction but was never implemented by either real client
//!   and was removed — see `docs/roadmap/hygiene-audit-2026-07-05.md`.)
//! - **`native`** — `SseDecoder`/`SseEvent` (gated: `#[cfg(not(target_arch = "wasm32"))]`).
//!   Pulls in `async-trait`, `tokio`, `futures`, `ulid` — not available on WASM. The incremental
//!   SSE framing parser, shared by the TUI and the `liberado chat` CLI client (each used to carry
//!   its own copy).
//!
//! The top-level `pub use wire::*` makes all wire types available at the crate root,
//! matching the old flat layout for existing import paths.

pub mod session_kind;
pub mod wire;

#[cfg(not(target_arch = "wasm32"))]
pub mod native;

// Re-export all wire types at the crate root for backward-compatible import paths.
pub use session_kind::{
    DomainWire, GoalHeaderResult, GoalHeaderSpec, SessionKind, SessionSummary, VisibilityWire,
};
pub use wire::*;
