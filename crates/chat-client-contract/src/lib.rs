//! Shared HTTP/SSE wire contract for Liberado chat clients.
//!
//! Every client (TUI, CLI, WebUI) and the server depend only on this crate for
//! the types they exchange over the API.
//!
//! # Module layout
//!
//! - **`wire`** — All wire DTOs (`DaemonStatus`, `ReactionEvent`, `ChatEvent`, etc.).
//!   Pure `serde` — no native deps, compiles to `wasm32-unknown-unknown`.
//! - **`native`** — `ChatClient` trait (gated: `#[cfg(not(target_arch = "wasm32"))]`).
//!   Pulls in `async-trait`, `tokio`, `futures`, `ulid` — not available on WASM.
//!
//! The top-level `pub use wire::*` makes all wire types available at the crate root,
//! matching the old flat layout for existing import paths.

pub mod wire;

#[cfg(not(target_arch = "wasm32"))]
pub mod native;

// Re-export all wire types at the crate root for backward-compatible import paths.
pub use wire::*;
