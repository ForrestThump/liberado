//! Git plumbing the runner needs, shelled out through `liberado_common::process`.
//!
//! Small total functions on purpose: each one is a single git invocation with an honest
//! error, so the CRAP ratchet stays cheap to satisfy and failures name their step.
//! Commits set identity explicitly — `user.email`/`user.name` exist on every dev machine
//! and on no CI runner or unattended box.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[error("git {args}: {stderr}")]
pub struct GitError {
    pub args: String,
    pub stderr: String,
}

async fn git(workspace: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = liberado_common::process::command("git")
        .current_dir(workspace)
        .args(args)
        .output()
        .await
        .map_err(|error| GitError {
            args: args.join(" "),
            stderr: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(GitError {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Fresh clones pin `core.autocrlf=false`: Windows line endings rewriting a whole diff
/// is a known trap the plan calls out (§7.2), and the pin is one config flag.
pub async fn clone(url: &str, destination: &Path) -> Result<(), GitError> {
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| GitError {
                args: format!("create_dir_all {}", parent.display()),
                stderr: error.to_string(),
            })?;
    }
    let dest = destination.to_string_lossy().into_owned();
    git(
        &PathBuf::from("."),
        &["clone", "--config", "core.autocrlf=false", url, &dest],
    )
    .await
    .map(|_| ())
}

pub async fn fetch(clone_dir: &Path) -> Result<(), GitError> {
    git(clone_dir, &["fetch", "origin", "--prune"])
        .await
        .map(|_| ())
}

/// Create `delegate-worktrees/<task>` at a new branch off `origin/<base_branch>`.
pub async fn create_worktree(
    clone_dir: &Path,
    branch: &str,
    worktree_path: &Path,
    base_branch: &str,
) -> Result<(), GitError> {
    let path = worktree_path.to_string_lossy().into_owned();
    git(
        clone_dir,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            &path,
            &format!("origin/{base_branch}"),
        ],
    )
    .await
    .map(|_| ())
}

/// Whether the worktree carries deliverable changes. Worker-local bookkeeping
/// ([`WORKER_LOCAL_PATHS`]) is excluded on purpose: those files are always untracked,
/// so counting them would report dirty forever and make a no-change re-run attempt
/// an empty commit.
pub fn is_dirty(worktree: &Path) -> bool {
    let excludes: Vec<String> = WORKER_LOCAL_PATHS
        .iter()
        .map(|path| format!(":(exclude){path}"))
        .collect();
    let mut args = vec!["status", "--porcelain", "--", "."];
    args.extend(excludes.iter().map(String::as_str));
    liberado_common::process::std_command("git")
        .current_dir(worktree)
        .args(&args)
        .output()
        .map(|output| !output.stdout.is_empty())
        .unwrap_or(false)
}

/// Paths the worker writes for its own bookkeeping. They document the run and stay on
/// the worker (plan §16); they are never part of the deliverable branch.
pub const WORKER_LOCAL_PATHS: &[&str] = &["coder-traces", ".liberado"];

pub async fn commit_all(worktree: &Path, message: &str) -> Result<(), GitError> {
    let excludes: Vec<String> = WORKER_LOCAL_PATHS
        .iter()
        .map(|path| format!(":(exclude){path}"))
        .collect();
    let mut add = vec!["add", "-A", "--", "."];
    add.extend(excludes.iter().map(String::as_str));
    git(worktree, &add).await?;
    git(
        worktree,
        &[
            "-c",
            "user.name=liberado-delegate",
            "-c",
            "user.email=delegate@liberado.local",
            "commit",
            "-m",
            message,
        ],
    )
    .await
    .map(|_| ())
}

pub async fn push(clone_dir: &Path, branch: &str) -> Result<(), GitError> {
    git(clone_dir, &["push", "-u", "origin", branch])
        .await
        .map(|_| ())
}
