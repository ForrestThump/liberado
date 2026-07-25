//! # liberado-session
//!
//! Domain-neutral **goal session** kernel for Liberado's agentic orchestration (scratchpad F).
//!
//! Surfaces (TUI, WebUI, CLI) are **clients**: they start goals, subscribe to
//! [`SessionEvent`] streams, and cancel — they never own tools, sandbox, or the loop.
//! Domain packs implement [`DomainPackRunner`] (coding, life-ops demo, …).
//!
//! See `docs/architecture/agentic-loops.md` and `docs/architecture/modularity.md`.

mod completion_gate;
mod event;
mod goal;
mod hub;
mod life_demo;
mod record_store;
mod runner;
mod store;

pub use completion_gate::{
    CompletionGate, GateError, GateEvidence, GateObserver, GateOutcome, GateVerdict, Quorum,
    RecordedVote, ReviewVote, Reviewer, ReviewerKind, VerifierStatus, VerifierVerdict,
};
pub use event::{SessionEvent, SessionEventKind};
pub use goal::{
    DomainHint, GoalResult, GoalSessionRecord, GoalSpec, SessionGrant, SessionOrigin,
    SessionStatus, TerminalKind, Visibility,
};
pub use hub::{GoalSessionHub, SendInputError, SessionAlert, SessionSnapshot};
pub use life_demo::LifeOpsDemoRunner;
pub use record_store::{SessionRecordStore, TurnAuthor};
pub use runner::{
    DomainPackRunner, HumanInput, InputChannel, InputOutcome, PackContext, PackError,
};
pub use store::GoalSessionStore;

/// Stable domain id for the life-ops demo pack (second-domain pigeonhole proof).
pub const LIFE_OPS_DOMAIN: &str = "life";
/// Stable domain id for the coding pack.
pub const CODING_DOMAIN: &str = "coding";
