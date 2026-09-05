//! Separate control-plane identities and durable ledger roots.
//!
//! Task, run, goal, provider-session, PR, revision, and GitHub-run stay distinct
//! so retries and later comparison never collapse two things into one string.
//! The worktree path is a lease on a record; the ledger lives under a repository
//! (or daemon) root that outlives a disposable worker tree.

use std::path::{Path, PathBuf};

use super::ControlPlaneError;

/// Shepherd is the only in-tree controller that may decide PR lifecycle.
pub const CONTROLLER_LIBERADO_SHEPHERD: &str = "liberado-shepherd";

/// External controller identity. Designed for a single lease per PR; not claimed here.
pub const CONTROLLER_GROK_BOT: &str = "grok-bot";

/// Repository-scoped durable home for task ledgers: `<repo>/.liberado/tasks`.
pub fn durable_tasks_root(repo_root: impl AsRef<Path>) -> PathBuf {
    repo_root.as_ref().join(".liberado").join("tasks")
}

/// Resolve a durable tasks root from a worktree lease.
///
/// A linked worktree uses the main repository root. A directory that is not a
/// git checkout falls back to that path so tests and scratch trees still work.
pub fn tasks_root_from_worktree(worktree: impl AsRef<Path>) -> PathBuf {
    git_repo_root(worktree.as_ref())
        .map(durable_tasks_root)
        .unwrap_or_else(|| durable_tasks_root(worktree.as_ref()))
}

/// Stable task id for one shepherded PR. Safe as a single path component.
pub fn shepherd_task_id(repository: Option<&str>, pr_number: u64) -> String {
    match repository {
        Some(repo) if !repo.trim().is_empty() => {
            let safe: String = repo
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            format!("pr-{safe}-{pr_number}")
        }
        _ => format!("pr-{pr_number}"),
    }
}

pub(super) fn validate_task_id(task_id: &str) -> Result<(), ControlPlaneError> {
    let path = Path::new(task_id);
    let mut components = path.components();
    let is_one_normal_component =
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none()
            && !task_id.trim().is_empty();
    if is_one_normal_component {
        Ok(())
    } else {
        Err(ControlPlaneError::InvalidTaskId(task_id.to_string()))
    }
}

fn git_repo_root(worktree: &Path) -> Option<PathBuf> {
    let output = liberado_common::process::std_command("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(worktree)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let git_dir = Path::new(raw);
    let git_dir = if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        worktree.join(git_dir)
    };
    git_dir.parent().map(Path::to_path_buf)
}
