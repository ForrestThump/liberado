//! Stable boundary between comparison policy and one coding harness.

use std::error::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterPreflight {
    pub harness: String,
    pub executable: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessExecution {
    pub harness: String,
    pub session_id: String,
    pub exit_code: i32,
}

/// One harness launch implementation.
///
/// The coordinator owns worktrees, common pins, the verifier, preservation, and result
/// classification. An adapter can only check its own executable and run inside the assigned
/// worktree. This prevents a harness-specific integration from changing experiment policy.
pub trait HarnessAdapter {
    fn id(&self) -> &'static str;

    /// The session id this adapter's runs are recorded under.
    fn session_id(&self) -> &str;

    fn preflight(&self) -> Result<AdapterPreflight, Box<dyn Error>>;

    fn launch(&self) -> Result<HarnessExecution, Box<dyn Error>>;

    /// Run one repair attempt with the given prompt, writing the harness-specific artifact stem.
    /// The prompt is passed the way this harness expects it (raw text for Liberado, an `@file`
    /// reference for pi).
    fn run(&self, prompt: &str, stem: &str) -> Result<i32, Box<dyn Error>>;
}
