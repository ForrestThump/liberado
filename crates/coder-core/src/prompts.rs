//! Harness prompts, editable without a rebuild.
//!
//! ## The problem
//!
//! Every prompt the coding harness uses lived in Rust: a 900-character string literal inside
//! `coder-runner`'s `run_headless`, a `const` for the diff reviewer, a function returning a
//! literal for the session critic. Tuning any of them meant a compile, and on this workspace a
//! compile is minutes. Prompt work is iterative by nature — the session critic went from missing
//! two of four labelled traces to four of four on one wording change — so the loop that most
//! wants to be fast was the slowest one available.
//!
//! Worse, two of those literals had drifted from `prompts/coder/coder.md`, which already existed
//! and already claimed to be the coder's prompt. Nobody could tell which text a given run used.
//!
//! ## How it works
//!
//! Each prompt has exactly one source of truth: a file under `prompts/coder/`. That file is
//! **baked in at compile time** with `include_str!` *and* **read from disk at run time** when it
//! is there.
//!
//! - In a checkout, the file on disk wins. Edit it, run again, no rebuild.
//! - In a container that ships only the binary, the baked copy is used and nothing breaks.
//!
//! Because both come from the same file, they cannot disagree about what the default is — the
//! baked copy is just an older snapshot of the same text, and only when the file is absent.
//!
//! ## Precedence
//!
//! A role's explicit `prompt` or `prompt_path` still outranks everything here; those are how a
//! deployment overrides one role. This module supplies the default that used to be a literal.

use std::path::{Path, PathBuf};

/// The coding worker's instructions.
pub const CODER: &str = include_str!("../../../prompts/coder/coder.md");
/// The cold reviewer that sees the diff and nothing else (completion-gate / attempt critic).
pub const DIFF_REVIEWER: &str = include_str!("../../../prompts/coder/diff-reviewer.md");
/// Product cold-PR stage (backlog 0.8): severity findings with code citations; no author context.
pub const COLD_PR_REVIEWER: &str = include_str!("../../../prompts/coder/cold-pr-reviewer.md");
/// The reviewer that reads a finished run's own narration.
pub const SESSION_CRITIC: &str = include_str!("../../../prompts/coder/session-critic.md");
/// The coding worker as the daemon session pack configures it, with self-host git rules.
pub const SESSION_PACK_CODER: &str = include_str!("../../../prompts/coder/session-pack-coder.md");
/// The criteria-intake planner that turns a rough writeup into an acceptance contract.
pub const INTAKE: &str = include_str!("../../../prompts/coder/intake.md");
/// Interactive ACP coding: conversation + tools, no `submit_report`.
pub const INTERACTIVE: &str = include_str!("../../../prompts/coder/interactive.md");

/// Where prompt files live relative to a checkout root.
pub const PROMPT_DIR: &str = "prompts/coder";

/// File name for each prompt, so the on-disk copy and the baked copy stay paired.
pub const CODER_FILE: &str = "coder.md";
pub const DIFF_REVIEWER_FILE: &str = "diff-reviewer.md";
pub const COLD_PR_REVIEWER_FILE: &str = "cold-pr-reviewer.md";
pub const SESSION_CRITIC_FILE: &str = "session-critic.md";
pub const SESSION_PACK_CODER_FILE: &str = "session-pack-coder.md";
pub const INTAKE_FILE: &str = "intake.md";
pub const INTERACTIVE_FILE: &str = "interactive.md";

/// Where to look for prompt files for a run on `workspace_root`.
///
/// An explicit `[coder] prompt_dir` wins. Otherwise it is `prompts/coder` **inside the workspace
/// the run is operating on** — not relative to the process's current directory.
///
/// That distinction is not pedantry. A coding run happens in a git worktree of this repo, so the
/// prompts are right there beside the code; the process's cwd, meanwhile, is whatever launched
/// the binary, which for the headless runner is arbitrary and for a `cargo test` is the crate
/// directory. Keying on cwd made the override work in exactly one situation and silently fall
/// back to the baked copy everywhere else — including inside the worktrees where a run would
/// most want the checkout's own prompts.
pub fn dir_for(configured: Option<&str>, workspace_root: &str) -> PathBuf {
    match configured {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(workspace_root).join(PROMPT_DIR),
    }
}

/// Load `file` from disk, falling back to `baked`.
///
/// Search order:
/// 1. `dir/file`, when a `[coder] prompt_dir` is configured.
/// 2. `prompts/coder/file` under the current directory — the ordinary checkout case.
/// 3. `baked`, the copy compiled in from that same file.
///
/// A file that exists but cannot be read is a **warning, not an error**. The alternative is
/// failing a coding run over a permissions problem on an optional override, which trades a
/// slightly-wrong prompt for no run at all.
///
/// An empty or whitespace-only file is treated as absent. Truncating a prompt to zero bytes is
/// far more likely to be an accident — an interrupted write, a bad mount — than an instruction
/// to run the model with no instructions.
pub fn load(dir: Option<&Path>, file: &str, baked: &'static str) -> String {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = dir {
        candidates.push(dir.join(file));
    }
    candidates.push(Path::new(PROMPT_DIR).join(file));

    for path in candidates {
        if let Some(text) = read_prompt_candidate(&path) {
            return text;
        }
    }
    baked.to_string()
}

/// Read one prompt candidate, logging the outcome. `None` means "keep looking": the file is
/// missing, unreadable, or empty, and the next candidate (or the baked copy) should be used.
fn read_prompt_candidate(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => {
            tracing::debug!(path = %path.display(), "loaded prompt from disk");
            Some(text)
        }
        Ok(_) => {
            tracing::warn!(
                path = %path.display(),
                "prompt file is empty; using the built-in copy"
            );
            None
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "prompt file exists but could not be read; using the built-in copy"
            );
            None
        }
    }
}

#[cfg(test)]
#[path = "prompts_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "prompts_guard_survivor_tests.rs"]
mod guard_survivor_tests;
