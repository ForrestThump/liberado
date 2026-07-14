//! # liberado-common
//!
//! The shared vocabulary of the Liberado system. Every other crate (daemon, dispatcher,
//! MCPs, hooks, TUI) compiles against these types, so this crate is deliberately
//! dependency-light and free of any I/O or runtime concerns — it is *types and pure
//! functions* only.
//!
//! Modules map onto the resolved architecture decisions:
//!
//! - [`capability`] — Zones, capabilities, and the narrow-only `CapabilitySet` (Decision 4,
//!   `liberado-permissions-idea.md`). The containment foundation.
//! - [`provenance`] — [`provenance::WriteProvenance`], attached to Turbovault audit entries
//!   so reactive consumers can attribute writes and break loops (Decision 5).
//! - [`event`] — the standardized [`event::Event`] payload that flows from every trigger
//!   source (vault subscription, timers, homelab hooks) into hooks (`life-os-architecture.md`
//!   §5).
//! - [`dispatch`] — [`dispatch::DispatchDecision`] / [`dispatch::Report`] and the execution
//!   model the dispatcher emits (Decision 1, `liberado-dispatch-logic-spec.md`).
//! - [`frontmatter`] — the YAML-frontmatter-fence note convention (render/extract), shared by
//!   every vault artifact that's structured metadata + a human-readable body: [`proposal`] here,
//!   `liberado_memory_store`'s notes, and `liberado-deliberate-mcp`'s deliberation transcripts.
//! - [`proposal`] — the [`proposal::Proposal`] human-in-the-loop artifact (Decision 11).
//! - [`model`] — [`model::ModelProfile`] + role capability floors (Decision 13).
//! - [`guidance`] — [`guidance::ToolGuidanceSource`], the dispatcher's procedural-memory seam
//!   (`liberado-dispatch-logic-spec.md` §2 steps 1/5), implemented by
//!   `liberado_memory_store::MemoryStore`.
//! - [`error`] — the crate's error type.
//!
//! The typed config model (Decision 14) used to live here as a `config` module — moved to
//! `liberado-config-loader` 2026-07-04 (`docs/roadmap/hygiene-audit-2026-07-04.md`), re-exported
//! from `liberado-config`. Nothing here reaches for it, so it doesn't belong in the shared
//! vocabulary every crate compiles against regardless of whether it touches config.

pub mod capability;
pub mod catalog;
pub mod dispatch;
pub mod error;
pub mod event;
pub mod frontmatter;
pub mod guidance;
pub mod model;
pub mod proposal;
pub mod provenance;

pub use capability::{
    Capability, CapabilitySet, Consequence, Magnitude, WriteClass, Zone, assess_magnitude,
    is_sweeping_destructive, mentions_destructive,
};
pub use catalog::{
    CapabilityCatalog, McpDescriptor, WriteTarget, resolve_zone, write_target,
    zone_write_restriction,
};
pub use dispatch::{
    BlockReason, DispatchAction, DispatchDecision, ExecMode, JobHandle, JobStatus, Outcome, Report,
    ToolCall, bare_tool_name, mcp_of,
};
pub use error::{Error, Result};
pub use event::{DEFAULT_POOL, Event, EventPayload, EventSource, event_source};
pub use frontmatter::{
    FRONTMATTER_FENCE, body_after_frontmatter, extract_frontmatter, render_note,
};
pub use guidance::{GuidanceHit, ToolGuidanceSource};
pub use model::{ModelChoice, ModelProfile, ModelRole, ModelTier, RequiredCaps};
pub use proposal::{
    PROPOSALS_DIR, Proposal, ProposalNoteError, ProposalSigner, ProposalStatus, ProposedAction,
    SignedProposal,
};
pub use provenance::{HUMAN_SOURCE, PROVENANCE_KEY, WriteProvenance};
