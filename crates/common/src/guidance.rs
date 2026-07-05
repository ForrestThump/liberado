//! The dispatcher's procedural-memory seam (`liberado-dispatch-logic-spec.md` §2, steps 1/5).
//!
//! `Dispatcher` depends only on this trait, never on `liberado-memory-store` directly, so the
//! dispatcher crate stays free of the vault/vector-search dependency stack. The concrete
//! implementation (`liberado_memory_store::MemoryStore`'s procedural store) is wired in by
//! whichever crate assembles the daemon (`liberado-bootstrap`), which already depends on both.

/// A single procedural-memory match: a stored "use tool X for task Y" directive plus which MCPs
/// it names, so the dispatcher can turn a high-confidence match into `relevant_mcps` — a hint,
/// never a replacement for the guard pipeline (Decision 1's asymmetric-cost principle: a wrong
/// hint should be cheap to recover from, not silently unsafe).
#[derive(Debug, Clone)]
pub struct GuidanceHit {
    pub content: String,
    pub tools_used: Vec<String>,
    pub score: f32,
}

/// The dispatcher's view of procedural memory: retrieve guidance before classification, record
/// outcomes after a decision resolves. Implemented by `liberado_memory_store::MemoryStore`'s
/// procedural store; `Dispatcher` holds this as `Option<Arc<dyn ToolGuidanceSource>>` so a
/// dispatcher built without one behaves exactly as it always has (RETRIEVE returns nothing,
/// RECORD is a no-op) — the seam is optional, not a hard dependency.
#[async_trait::async_trait]
pub trait ToolGuidanceSource: Send + Sync {
    /// Retrieve guidance related to `goal`, most relevant first. Empty when nothing matches, or
    /// on any backend failure — retrieval failures degrade to "no guidance found," never abort
    /// the dispatch (the classify step is the source of truth either way).
    async fn search_tool_guidance(&self, goal: &str) -> Vec<GuidanceHit>;

    /// Record a new guidance directive. Best-effort: the caller does not treat a failure here as
    /// fatal to the dispatch that produced it (the decision has already been made and acted on;
    /// this only affects future dispatches).
    async fn save_tool_guidance(
        &self,
        directive: &str,
        task_type: Option<String>,
        tools_used: Vec<String>,
    );
}
