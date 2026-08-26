//! LAN delegation worker (`docs/future-work/delegate-network-plan.md` §7, D1 slice).
//!
//! The worker hosts the control plane: an axum HTTP server speaking
//! [`liberado_delegate_contract`], a durable task queue on local disk, and the runner that
//! executes one delegated task through the same coding pack a local fan-out child uses —
//! `assemble_production_run`, executor in report mode, coder-traces per turn. Nothing here
//! is a second agent engine; "different runtime + different verifiers" is banned by the
//! architecture contracts.
//!
//! D1 scope, honestly:
//!
//! - accept → clone → worktree → run → push branch → open PR; duplicate submit is a no-op.
//! - Cancel is queue-level: queued tasks cancel cleanly, running tasks refuse with 409
//!   until cooperative stop lands with park/resume (D2).
//! - Acceptance gates travel in the [`liberado_delegate_contract::TaskSpec`] but are not
//!   yet enforced (D3); no disk-floor check at accept time yet.
//! - Token auth on every route, constant-time compared; LAN-only by design, no discovery.
//!
//! The model comes from the worker's own `[[providers]]` topology entry: the selected
//! profile's `default_model` names what delegated runs use unless `--model` overrides it.
//! For unattended boxes where the bill is the failure mode, point the profile at
//! `liberado-free-proxy` (`base_url = "http://127.0.0.1:8788/v1"`,
//! `default_model = "auto"`) and every delegated run resolves to the best-ranked free
//! model — the proxy refuses paid slugs loudly, so delegation cannot silently cost money.

pub mod ask;
pub mod cli;
pub mod config;
pub mod git;
pub mod http;
pub mod inbox;
pub mod provider_factory;
pub mod queue;
pub mod runner;

/// Crate version + build-time `git describe`; surfaced through `/health`.
pub fn build_fingerprint() -> String {
    format!(
        "{}+{}",
        env!("CARGO_PKG_VERSION"),
        env!("LIBERADO_BUILD_FINGERPRINT")
    )
}
