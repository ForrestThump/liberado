//! # liberado-tool-runtime
//!
//! The tool-runtime **contract**, sunk to the foundation layer. `liberado-executor` (the loop)
//! drives a [`ToolRuntime`]; `liberado-mcp` (the production adapter) implements it;
//! `liberado-test-support` (the shared test doubles) implements it too. The trait items used to
//! live in `executor` and `mcp` themselves — which forced every consumer of the contract to depend
//! on a full engine or adapter, and made the shared test doubles unusable inside those crates' own
//! unit tests: a crate's `#[cfg(test)]` binary is a *second* rustc instance of that crate, so a
//! double compiled against the normal instance implements a different trait than the one the unit
//! test sees. Traits that doubles must implement must live below every crate they serve.
//!
//! - [`ToolRuntime`] — the catalog the model is offered plus how one call is executed.
//! - [`RuntimeFactory`] / [`RuntimeSetupError`] — how an orchestrator obtains a connected runtime
//!   for an execution, scoped to the MCPs it may see and the provenance every call must carry.
//! - [`RebindableRuntime`] — the pool-facing extension: accept a new execution's write provenance
//!   without reconnecting, report transport death, shut down asynchronously.
//! - [`DecoratingRuntime`] — the shared "passthrough + one tiny rule" wrapper used by every
//!   `impl ToolRuntime` whose only job is to filter the catalog or short-circuit one
//!   `invoke` (e.g. `PassThroughRuntime`, `ScopedRuntime`).

use std::path::PathBuf;

use async_trait::async_trait;
use liberado_common::WriteProvenance;
use liberado_provider::{ToolDef, ToolInvocation};
use thiserror::Error;

mod decorating;
pub use decorating::DecoratingRuntime;

/// The tools available for a run plus how to execute them. Implemented by the turbomcp-backed
/// runtime in production and by the `liberado-test-support` doubles in tests; the engine depends
/// only on this.
#[async_trait]
pub trait ToolRuntime: Send + Sync {
    /// The tool catalog offered to the model this run. Capability narrowing happens *here* (a
    /// subagent's runtime only lists tools it is permitted to call), which is why the dispatcher's
    /// pre-flight guard over the classifier's opening move is a check, not the boundary.
    fn catalog(&self) -> Vec<ToolDef>;

    /// Execute one model-requested call and return the textual result fed back to the model.
    ///
    /// A **tool-level** failure is returned as `Err(message)`: the engine surfaces it to the model
    /// in-band (as the tool result) so it can adapt, exactly as a real agent would. Reserve hard
    /// errors (which abort the whole loop) for infrastructure faults, by surfacing them through the
    /// runtime's own state rather than here.
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String>;

    /// Whether a tool is safe to run concurrently with other tool calls in the same turn.
    /// Read-only tools (file reads, searches, git inspection) return true.
    /// Default: false (conservative — treat every tool as potentially stateful).
    fn is_read_only(&self, _tool_name: &str) -> bool {
        false
    }

    /// After this tool returns, stop the conversational loop and wait for the
    /// human's next message. The tool result is *not* written until that answer
    /// arrives (ACP cannot overlap two `session/prompt`s).
    fn parks_for_human(&self, _tool_name: &str) -> bool {
        false
    }
}

/// Failure building a [`ToolRuntime`] for an execution (connection/handshake/etc.).
#[derive(Debug, Error)]
#[error("{0}")]
pub struct RuntimeSetupError(pub String);

/// How an orchestrator obtains a [`ToolRuntime`] for an execution: given the MCPs the execution is
/// allowed to see and the provenance every call should carry, return a connected runtime. The real
/// implementation (turbomcp-backed) lives in the MCP layer; tests inject a double. Lives in this
/// foundation crate — not in the orchestrator that consumes it, nor in the executor or MCP crates
/// that used to host it — so the engine, the adapter, and the test doubles all implement the same
/// trait instance.
#[async_trait]
pub trait RuntimeFactory: Send + Sync {
    async fn runtime_for(
        &self,
        allowed_mcps: &[String],
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError>;

    /// Like [`runtime_for`](Self::runtime_for), but scoped to a per-worker workspace root.
    ///
    /// `workspace_root` is `Some(path)` when the worker must operate inside an isolated
    /// filesystem workspace (a git worktree, in the coding pack's world) and `None` when the
    /// worker is unconstrained. The **default** implementation ignores the root and behaves
    /// exactly like [`runtime_for`](Self::runtime_for) — factories that do not care about
    /// workspace isolation (the MCP registry, test doubles) never need to override this.
    ///
    /// Placement (backlog C7): the *seam* is kernel-side — an orchestrator that fans work out
    /// passes the root through untouched — but the *isolation* is a pack concern. The concrete
    /// worktree primitive lives in `coder-sandbox` (pack); the production caller builds the
    /// workspaces and supplies a factory that roots each worker's runtime in one. The kernel
    /// never reaches across the layer line for the primitive itself.
    async fn runtime_for_in(
        &self,
        allowed_mcps: &[String],
        provenance: WriteProvenance,
        workspace_root: Option<PathBuf>,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        let _ = workspace_root;
        self.runtime_for(allowed_mcps, provenance).await
    }
}

/// A [`ToolRuntime`] that can accept a new execution's write provenance without reconnecting.
///
/// Pooling reuses the underlying MCP session/catalog; provenance (Decision 5 correlation) must
/// still be per-execution — implementors update whatever they inject into tool `_meta`.
#[async_trait]
pub trait RebindableRuntime: ToolRuntime {
    fn rebind_provenance(&mut self, provenance: WriteProvenance);

    /// `true` when the last failure was a **connection/transport** error (not an in-band tool
    /// `isError`). Pooled checkouts use this so dead peers are not checked back in.
    fn connection_is_dead(&self) -> bool {
        false
    }

    /// Gracefully shut down the underlying connection and release transport resources.
    ///
    /// Called (on a spawned task, since teardown needs `await`) before a pooled runtime is
    /// discarded — idle reap, dead checkout, or invalidate — so HTTP SSE tasks are aborted and the
    /// server-side session is terminated. Without this, a bare sync `Drop` leaks pooled HTTP
    /// connections and they pile up server-side. Default is a no-op for peers with no connection
    /// to tear down (stdio children drop their process on `Drop`).
    async fn shutdown(&mut self) {}
}
