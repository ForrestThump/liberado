//! # liberado-session
//!
//! Domain-neutral **goal session** kernel for Liberado's agentic mesh (scratchpad F).
//!
//! Surfaces (TUI, WebUI, CLI) are **clients**: they start goals, subscribe to
//! [`SessionEvent`] streams, and cancel — they never own tools, sandbox, or the loop.
//! Domain packs implement [`DomainPackRunner`] (coding, life-ops demo, …).
//!
//! See `docs/architecture/agentic-loops.md` and `docs/architecture/modularity.md`.

mod event;
mod goal;
mod hub;
mod life_demo;
mod runner;
mod store;

pub use event::{SessionEvent, SessionEventKind};
pub use goal::{DomainHint, GoalResult, GoalSpec, SessionStatus, TerminalKind};
pub use hub::{GoalSessionHub, SessionSnapshot};
pub use life_demo::LifeOpsDemoRunner;
pub use runner::{DomainPackRunner, HumanInput, InputChannel, InputOutcome, PackError};
pub use store::GoalSessionStore;

/// Stable domain id for the life-ops demo pack (second-domain pigeonhole proof).
pub const LIFE_OPS_DOMAIN: &str = "life";
/// Stable domain id for the coding pack.
pub const CODING_DOMAIN: &str = "coding";
