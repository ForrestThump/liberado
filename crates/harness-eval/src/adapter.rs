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

/// Default PATH program for an external harness. Liberado is built from the pinned worktree
/// and has no PATH default.
pub fn default_path_program(harness_id: &str) -> Option<&'static str> {
    match harness_id {
        "pi" => Some(if cfg!(windows) { "pi.cmd" } else { "pi" }),
        "hermes" => Some(if cfg!(windows) {
            "hermes.exe"
        } else {
            "hermes"
        }),
        // Coding CLI from langchain-ai/deepagents (`deepagents-code` / `dcode`).
        "deepagents" => Some(if cfg!(windows) { "dcode.exe" } else { "dcode" }),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::default_path_program;

    #[test]
    fn path_defaults_cover_the_external_c3_harnesses() {
        assert_eq!(
            default_path_program("pi"),
            Some(if cfg!(windows) { "pi.cmd" } else { "pi" })
        );
        assert_eq!(
            default_path_program("hermes"),
            Some(if cfg!(windows) {
                "hermes.exe"
            } else {
                "hermes"
            })
        );
        assert_eq!(
            default_path_program("deepagents"),
            Some(if cfg!(windows) { "dcode.exe" } else { "dcode" })
        );
        assert!(default_path_program("liberado").is_none());
        assert!(default_path_program("cline").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn python_tool_installers_use_windows_executables() {
        assert_eq!(default_path_program("hermes"), Some("hermes.exe"));
        assert_eq!(default_path_program("deepagents"), Some("dcode.exe"));
    }
}
