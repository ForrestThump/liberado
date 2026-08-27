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
    let excludes = pathspec_excludes_for(worktree);
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

/// `:(exclude)` pathspecs for worker-local dirs that are **not** already gitignored.
///
/// Naming an ignored path in a `git add` pathspec (even as `:(exclude)…`) makes git
/// exit non-zero ("paths are ignored by one of your .gitignore files"). Real Liberado
/// trees gitignore `coder-traces/`; the stub dogfood repo did not — so always-on
/// excludes worked until the 213 pin dogfood. Only exclude paths git would otherwise stage.
fn pathspec_excludes_for(worktree: &Path) -> Vec<String> {
    WORKER_LOCAL_PATHS
        .iter()
        .filter(|path| !is_ignored(worktree, path))
        .map(|path| format!(":(exclude){path}"))
        .collect()
}

fn is_ignored(worktree: &Path, path: &str) -> bool {
    liberado_common::process::std_command("git")
        .current_dir(worktree)
        .args(["check-ignore", "-q", path])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub async fn commit_all(worktree: &Path, message: &str) -> Result<(), GitError> {
    let excludes = pathspec_excludes_for(worktree);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn sh(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Regression for Liberado trees that gitignore `coder-traces/`: naming that path
    /// in a `git add` pathspec used to fail commit_all even though we only meant to exclude it.
    #[tokio::test]
    async fn commit_all_when_local_paths_are_gitignored() {
        let root = tempfile::tempdir().expect("tempdir");
        let repo = root.path();
        sh(repo, &["init", "-q", "-b", "main"]);
        sh(repo, &["config", "user.email", "test@liberado.local"]);
        sh(repo, &["config", "user.name", "test"]);
        std::fs::write(repo.join(".gitignore"), "coder-traces/\n.liberado/\n").unwrap();
        std::fs::write(repo.join("README.md"), "seed\n").unwrap();
        sh(repo, &["add", "-A"]);
        sh(repo, &["commit", "-q", "-m", "seed"]);

        std::fs::create_dir_all(repo.join("coder-traces")).unwrap();
        std::fs::write(repo.join("coder-traces/session.json"), "{}\n").unwrap();
        std::fs::write(repo.join("delivered.txt"), "hi\n").unwrap();

        commit_all(repo, "deliver")
            .await
            .expect("commit_all must succeed when coder-traces is gitignored");

        let tree = Command::new("git")
            .current_dir(repo)
            .args(["ls-tree", "-r", "--name-only", "HEAD"])
            .output()
            .expect("ls-tree");
        let names = String::from_utf8_lossy(&tree.stdout);
        assert!(names.contains("delivered.txt"), "{names}");
        assert!(!names.contains("coder-traces"), "{names}");
    }

    #[tokio::test]
    async fn commit_all_still_skips_unignored_local_paths() {
        let root = tempfile::tempdir().expect("tempdir");
        let repo = root.path();
        sh(repo, &["init", "-q", "-b", "main"]);
        sh(repo, &["config", "user.email", "test@liberado.local"]);
        sh(repo, &["config", "user.name", "test"]);
        std::fs::write(repo.join("README.md"), "seed\n").unwrap();
        sh(repo, &["add", "-A"]);
        sh(repo, &["commit", "-q", "-m", "seed"]);

        std::fs::create_dir_all(repo.join("coder-traces")).unwrap();
        std::fs::write(repo.join("coder-traces/session.json"), "{}\n").unwrap();
        std::fs::write(repo.join("delivered.txt"), "hi\n").unwrap();

        commit_all(repo, "deliver")
            .await
            .expect("commit_all must succeed when coder-traces is not gitignored");

        let tree = Command::new("git")
            .current_dir(repo)
            .args(["ls-tree", "-r", "--name-only", "HEAD"])
            .output()
            .expect("ls-tree");
        let names = String::from_utf8_lossy(&tree.stdout);
        assert!(names.contains("delivered.txt"), "{names}");
        assert!(!names.contains("coder-traces"), "{names}");
    }
}
