//! Parent-side git merge helpers for coding-subagent fan-out (S6 / C7).
//!
//! Children never self-merge. The parent integrates each child's branch tip; on conflict the
//! caller (coding pack) runs LLM-assisted resolution, then finishes the merge.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use thiserror::Error;

use crate::{path_for_cli, strip_extended_path_prefix};

#[derive(Debug, Error)]
pub enum MergeError {
    #[error("git failed: {0}")]
    Git(String),
    #[error("io: {0}")]
    Io(String),
}

/// Result of attempting to merge `branch` into the current HEAD of `repo_root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeAttempt {
    /// Fast-forward or merge commit succeeded with no conflicts.
    Clean { merge_commit: Option<String> },
    /// Merge stopped with conflicted paths; index still mid-merge.
    Conflicts { paths: Vec<String> },
}

/// Serializes every mutation of a repository's worktree registry.
///
/// `git worktree prune`, `git branch -D`, `git worktree add` and `git worktree remove`
/// all rewrite `.git/worktrees/`, and git does not write that metadata atomically. Two
/// children setting up at the same moment produced this on a Windows CI runner:
///
/// ```text
/// fatal: failed to read .git/worktrees/fanout-api-0/commondir: No error
/// ```
///
/// One child's `prune` was rewriting the directory another child's `add` was reading. It passed
/// on Linux and locally, and failed roughly one run in ten on Windows — twice, and the first time
/// I recorded it as an unexplained flake because I could not reproduce it in five local runs.
///
/// `remove_worktree` must take the same lock. Its fallback is `remove_dir_all` + `prune`,
/// and an unlocked remove racing a sibling `add` is how a fan-out child fails setup
/// before its backend call — even when `max_concurrent` is 1, because other tasks in this
/// process still add and remove. Ubuntu CI saw that as `calls == 1` instead of `2`.
///
/// A single global lock rather than one per repository: creating a worktree takes milliseconds,
/// the concurrency that matters is the coding work that follows, and two unrelated repositories
/// contending for a few milliseconds is not worth a keyed map to avoid.
static WORKTREE_REGISTRY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Number of tasks inside the guarded section. Test-only, and the only way to assert the lock is
/// doing its job — a race fix whose test is "run it a lot and hope" proves nothing.
///
/// [`PEAK_IN_REGISTRY`] is updated on *enter*, not after the function returns. Sampling the
/// count after `add`/`remove` completes only sees whoever is still inside; an unlocked
/// `remove` overlapping a locked `add` is then invisible (the add holds the mutex, so at most
/// one add remains, and the overlapping remove has already dropped).
#[cfg(test)]
pub(crate) static CONCURRENT_IN_REGISTRY: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(crate) static PEAK_IN_REGISTRY: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Create a linked worktree on a **named branch** at `parent` HEAD.
///
/// `branch` must be a safe ref name (no path separators / `..`). The branch is created if missing
/// (`git worktree add -b <branch> <path>`). Returns the worktree path.
pub async fn add_worktree_on_branch(
    parent_root: &Path,
    worktrees_base: &Path,
    worktree_name: &str,
    branch: &str,
) -> Result<PathBuf, MergeError> {
    validate_safe_name(worktree_name, "worktree name")?;
    validate_branch_name(branch)?;

    let parent_root = strip_extended_path_prefix(
        &parent_root
            .canonicalize()
            .map_err(|e| MergeError::Io(e.to_string()))?,
    );
    let worktrees_base = strip_extended_path_prefix(worktrees_base);
    let dest = worktrees_base.join(worktree_name);
    std::fs::create_dir_all(&worktrees_base).map_err(|e| MergeError::Io(e.to_string()))?;

    let parent_cli = path_for_cli(&parent_root);
    let dest_cli = path_for_cli(&dest);

    // Everything below rewrites `.git/worktrees/`. Held across all three git calls, not just the
    // add: the failure was a sibling's `prune` running mid-`add`.
    let _registry = WORKTREE_REGISTRY.lock().await;
    #[cfg(test)]
    let _depth = ConcurrencyProbe::enter();

    let _ = liberado_common::process::command("git")
        .args(["-C", &parent_cli, "worktree", "prune"])
        .output()
        .await;

    if dest.exists() {
        let _ = std::fs::remove_dir_all(&dest);
    }

    // Remove stale branch if it exists from a prior crashed run (only if not checked out).
    let _ = liberado_common::process::command("git")
        .args(["-C", &parent_cli, "branch", "-D", branch])
        .output()
        .await;

    let output = git_worktree_add(&parent_cli, branch, &dest_cli).await?;
    if output.status.success() {
        return Ok(dest);
    }
    // Held lock, dest cleaned, branch deleted — a leftover `index.lock` or a dest
    // that reappeared still fails the first add. One retry after a second
    // prune/cleanup is the mutation-site retry; do not retry the whole caller.
    retry_worktree_add(&parent_cli, branch, &dest, &dest_cli, &output.stderr).await
}

/// Remove a worktree path and prune registrations (branch is left intact for merge).
pub async fn remove_worktree(parent_root: &Path, worktree_path: &Path) -> Result<(), MergeError> {
    // Same registry as add: remove + fallback prune rewrite `.git/worktrees/` and
    // may `remove_dir_all` a dest another add is writing.
    let _registry = WORKTREE_REGISTRY.lock().await;
    #[cfg(test)]
    let _depth = ConcurrencyProbe::enter();

    let parent_cli = path_for_cli(&strip_extended_path_prefix(parent_root));
    let dest_cli = path_for_cli(&strip_extended_path_prefix(worktree_path));
    let output = liberado_common::process::command("git")
        .args([
            "-C",
            &parent_cli,
            "worktree",
            "remove",
            "--force",
            &dest_cli,
        ])
        .output()
        .await
        .map_err(|e| MergeError::Git(format!("worktree remove: {e}")))?;
    if !output.status.success() {
        // Fall back to directory delete + prune.
        let _ = std::fs::remove_dir_all(worktree_path);
        let _ = liberado_common::process::command("git")
            .args(["-C", &parent_cli, "worktree", "prune"])
            .output()
            .await;
    }
    Ok(())
}

/// Merge `branch` into HEAD of `repo_root` (must be a git checkout, not bare).
pub async fn merge_branch(repo_root: &Path, branch: &str) -> Result<MergeAttempt, MergeError> {
    validate_branch_name(branch)?;
    let repo_cli = path_for_cli(&strip_extended_path_prefix(repo_root));

    // Abort any leftover merge state.
    let _ = liberado_common::process::command("git")
        .args(["-C", &repo_cli, "merge", "--abort"])
        .output()
        .await;

    let output = liberado_common::process::command("git")
        .args([
            "-C",
            &repo_cli,
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            &format!("merge coding subagent branch {branch}"),
            branch,
        ])
        .output()
        .await
        .map_err(|e| MergeError::Git(format!("merge: {e}")))?;

    if output.status.success() {
        let sha = rev_parse(repo_root, "HEAD").await.ok();
        return Ok(MergeAttempt::Clean { merge_commit: sha });
    }

    let conflicts = list_unmerged_paths(repo_root).await?;
    if conflicts.is_empty() {
        return Err(MergeError::Git(format!(
            "merge failed without conflicts: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(MergeAttempt::Conflicts { paths: conflicts })
}

/// Paths with unmerged index entries (conflicted files).
pub async fn list_unmerged_paths(repo_root: &Path) -> Result<Vec<String>, MergeError> {
    let repo_cli = path_for_cli(&strip_extended_path_prefix(repo_root));
    let output = liberado_common::process::command("git")
        .args(["-C", &repo_cli, "diff", "--name-only", "--diff-filter=U"])
        .output()
        .await
        .map_err(|e| MergeError::Git(format!("diff unmerged: {e}")))?;
    if !output.status.success() {
        return Err(MergeError::Git(format!(
            "list unmerged failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Read file contents for merge resolution: ours / theirs / combined conflict file.
#[derive(Debug, Clone)]
pub struct ConflictSides {
    pub path: String,
    pub ours: String,
    pub theirs: String,
    pub combined: String,
}

pub async fn read_conflict_sides(
    repo_root: &Path,
    rel_path: &str,
) -> Result<ConflictSides, MergeError> {
    let repo_cli = path_for_cli(&strip_extended_path_prefix(repo_root));
    let ours = git_show(&repo_cli, ":2", rel_path)
        .await
        .unwrap_or_default();
    let theirs = git_show(&repo_cli, ":3", rel_path)
        .await
        .unwrap_or_default();
    let full = strip_extended_path_prefix(repo_root).join(rel_path);
    let combined = std::fs::read_to_string(&full).unwrap_or_default();
    Ok(ConflictSides {
        path: rel_path.to_string(),
        ours,
        theirs,
        combined,
    })
}

/// Write resolved content and `git add` the path (stage for merge continue).
pub async fn stage_resolution(
    repo_root: &Path,
    rel_path: &str,
    content: &str,
) -> Result<(), MergeError> {
    let root = strip_extended_path_prefix(repo_root);
    let full = root.join(rel_path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MergeError::Io(e.to_string()))?;
    }
    std::fs::write(&full, content).map_err(|e| MergeError::Io(e.to_string()))?;
    let repo_cli = path_for_cli(&root);
    let output = liberado_common::process::command("git")
        .args(["-C", &repo_cli, "add", "--", rel_path])
        .output()
        .await
        .map_err(|e| MergeError::Git(format!("git add: {e}")))?;
    if !output.status.success() {
        return Err(MergeError::Git(format!(
            "git add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

/// Complete a merge after all conflicts are staged (`git commit` with no-edit message).
pub async fn commit_merge(repo_root: &Path, message: &str) -> Result<String, MergeError> {
    let repo_cli = path_for_cli(&strip_extended_path_prefix(repo_root));
    let output = liberado_common::process::command("git")
        .args(["-C", &repo_cli, "commit", "--no-edit", "-m", message])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| MergeError::Git(format!("commit merge: {e}")))?;
    if !output.status.success() {
        // Maybe already committed via merge --continue style; try status.
        return Err(MergeError::Git(format!(
            "commit merge failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    rev_parse(repo_root, "HEAD").await
}

pub async fn rev_parse(repo_root: &Path, rev: &str) -> Result<String, MergeError> {
    let repo_cli = path_for_cli(&strip_extended_path_prefix(repo_root));
    let output = liberado_common::process::command("git")
        .args(["-C", &repo_cli, "rev-parse", rev])
        .output()
        .await
        .map_err(|e| MergeError::Git(format!("rev-parse: {e}")))?;
    if !output.status.success() {
        return Err(MergeError::Git(format!(
            "rev-parse {rev}: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn branch_tip(repo_root: &Path, branch: &str) -> Result<String, MergeError> {
    rev_parse(repo_root, branch).await
}

async fn git_show(repo_cli: &str, stage: &str, path: &str) -> Result<String, MergeError> {
    let spec = format!("{stage}:{path}");
    let output = liberado_common::process::command("git")
        .args(["-C", repo_cli, "show", &spec])
        .output()
        .await
        .map_err(|e| MergeError::Git(format!("git show: {e}")))?;
    if !output.status.success() {
        return Err(MergeError::Git(format!(
            "git show {spec}: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn validate_safe_name(name: &str, what: &str) -> Result<(), MergeError> {
    if name.is_empty()
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.starts_with('-')
    {
        return Err(MergeError::Io(format!(
            "{what} '{name}' is not a safe directory name"
        )));
    }
    Ok(())
}

async fn git_worktree_add(
    parent_cli: &str,
    branch: &str,
    dest_cli: &str,
) -> Result<std::process::Output, MergeError> {
    liberado_common::process::command("git")
        .args([
            "-C", parent_cli, "worktree", "add", "-b", branch, dest_cli, "HEAD",
        ])
        .output()
        .await
        .map_err(|e| MergeError::Git(format!("worktree add: {e}")))
}

fn worktree_add_failed(stderr: &[u8]) -> MergeError {
    MergeError::Git(format!(
        "worktree add -b failed: {}",
        String::from_utf8_lossy(stderr)
    ))
}

/// Transient git failures that a second add, still under [`WORKTREE_REGISTRY`], can clear.
///
/// Kept as a named predicate so the retry arm is a real branch a test can invert,
/// not a string compare buried inside `add_worktree_on_branch`.
fn worktree_add_is_retryable(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("index.lock")
        || s.contains("unable to create")
        || s.contains("already exists")
        || s.contains("failed to read .git/worktrees")
        || s.contains("is already used by worktree")
}

async fn retry_worktree_add(
    parent_cli: &str,
    branch: &str,
    dest: &Path,
    dest_cli: &str,
    first_stderr: &[u8],
) -> Result<PathBuf, MergeError> {
    let stderr = String::from_utf8_lossy(first_stderr);
    if !worktree_add_is_retryable(&stderr) {
        return Err(worktree_add_failed(first_stderr));
    }
    let _ = liberado_common::process::command("git")
        .args(["-C", parent_cli, "worktree", "prune"])
        .output()
        .await;
    if dest.exists() {
        let _ = std::fs::remove_dir_all(dest);
        let _ = std::fs::remove_file(dest);
    }
    let _ = liberado_common::process::command("git")
        .args(["-C", parent_cli, "branch", "-D", branch])
        .output()
        .await;
    let retry = git_worktree_add(parent_cli, branch, dest_cli).await?;
    if retry.status.success() {
        return Ok(dest.to_path_buf());
    }
    Err(worktree_add_failed(&retry.stderr))
}

fn validate_branch_name(branch: &str) -> Result<(), MergeError> {
    if branch.is_empty()
        || branch.contains("..")
        || branch.contains('\\')
        || branch.starts_with('-')
        || branch.contains(' ')
    {
        return Err(MergeError::Io(format!(
            "branch '{branch}' is not a safe git ref name"
        )));
    }
    // Allow fanout/label-style paths with single slashes.
    if branch.split('/').any(|p| p.is_empty()) {
        return Err(MergeError::Io(format!(
            "branch '{branch}' has empty path segment"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "merge_validation_tests.rs"]
mod validation_tests;

/// Increments [`CONCURRENT_IN_REGISTRY`] on construction and decrements on drop, so a test can
/// assert the guarded section is never entered twice at once.
#[cfg(test)]
struct ConcurrencyProbe;

#[cfg(test)]
impl ConcurrencyProbe {
    fn enter() -> Self {
        let now = CONCURRENT_IN_REGISTRY.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        PEAK_IN_REGISTRY.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

#[cfg(test)]
impl Drop for ConcurrencyProbe {
    fn drop(&mut self) {
        CONCURRENT_IN_REGISTRY.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "merge_registry_lock_tests.rs"]
mod registry_lock_tests;
