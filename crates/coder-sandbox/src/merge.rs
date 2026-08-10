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
/// `git worktree prune`, `git branch -D` and `git worktree add` all rewrite `.git/worktrees/`,
/// and git does not write that metadata atomically. Two children setting up at the same moment
/// produced this on a Windows CI runner:
///
/// ```text
/// fatal: failed to read .git/worktrees/fanout-api-0/commondir: No error
/// ```
///
/// One child's `prune` was rewriting the directory another child's `add` was reading. It passed
/// on Linux and locally, and failed roughly one run in ten on Windows — twice, and the first time
/// I recorded it as an unexplained flake because I could not reproduce it in five local runs.
///
/// A single global lock rather than one per repository: creating a worktree takes milliseconds,
/// the concurrency that matters is the coding work that follows, and two unrelated repositories
/// contending for a few milliseconds is not worth a keyed map to avoid.
static WORKTREE_REGISTRY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Number of tasks inside the guarded section. Test-only, and the only way to assert the lock is
/// doing its job — a race fix whose test is "run it a lot and hope" proves nothing.
#[cfg(test)]
pub(crate) static CONCURRENT_IN_REGISTRY: std::sync::atomic::AtomicUsize =
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

    let output = liberado_common::process::command("git")
        .args([
            "-C",
            &parent_cli,
            "worktree",
            "add",
            "-b",
            branch,
            &dest_cli,
            "HEAD",
        ])
        .output()
        .await
        .map_err(|e| MergeError::Git(format!("worktree add: {e}")))?;
    if !output.status.success() {
        return Err(MergeError::Git(format!(
            "worktree add -b failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(dest)
}

/// Remove a worktree path and prune registrations (branch is left intact for merge).
pub async fn remove_worktree(parent_root: &Path, worktree_path: &Path) -> Result<(), MergeError> {
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
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "--quiet"]);
        git(dir, &["config", "user.email", "test@liberado.local"]);
        git(dir, &["config", "user.name", "liberado-test"]);
        std::fs::write(dir.join("README.md"), "base\n").unwrap();
        git(dir, &["add", "README.md"]);
        git(dir, &["commit", "-m", "base", "--quiet"]);
    }

    #[tokio::test]
    async fn worktree_on_branch_then_clean_merge() {
        let root = std::env::temp_dir().join(format!("lib-merge-clean-{}", unique()));
        let wt_base = root.join("wts");
        init_repo(&root);

        let wt = add_worktree_on_branch(&root, &wt_base, "child-a", "fanout/a")
            .await
            .unwrap();
        std::fs::write(wt.join("a.txt"), "from-a\n").unwrap();
        git(&wt, &["add", "a.txt"]);
        git(&wt, &["commit", "-m", "a", "--quiet"]);
        remove_worktree(&root, &wt).await.unwrap();

        match merge_branch(&root, "fanout/a").await.unwrap() {
            MergeAttempt::Clean { .. } => {}
            other => panic!("expected clean merge, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap().trim(),
            "from-a"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn conflicting_merge_lists_paths_and_resolves() {
        let root = std::env::temp_dir().join(format!("lib-merge-conflict-{}", unique()));
        let wt_base = root.join("wts");
        init_repo(&root);

        // Branch B edits README.
        let wt = add_worktree_on_branch(&root, &wt_base, "child-b", "fanout/b")
            .await
            .unwrap();
        std::fs::write(wt.join("README.md"), "branch-b\n").unwrap();
        git(&wt, &["add", "README.md"]);
        git(&wt, &["commit", "-m", "b", "--quiet"]);
        remove_worktree(&root, &wt).await.unwrap();

        // Parent also edits README.
        std::fs::write(root.join("README.md"), "parent\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-m", "parent", "--quiet"]);

        match merge_branch(&root, "fanout/b").await.unwrap() {
            MergeAttempt::Conflicts { paths } => {
                assert!(
                    paths.iter().any(|p| p.contains("README")),
                    "expected README conflict, got {paths:?}"
                );
            }
            other => panic!("expected conflicts, got {other:?}"),
        }

        stage_resolution(&root, "README.md", "resolved\n")
            .await
            .unwrap();
        let sha = commit_merge(&root, "resolve conflict").await.unwrap();
        assert!(!sha.is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("README.md"))
                .unwrap()
                .trim(),
            "resolved"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn read_conflict_sides_returns_ours_and_theirs() {
        let root = std::env::temp_dir().join(format!("lib-merge-sides-{}", unique()));
        let wt_base = root.join("wts");
        init_repo(&root);

        // Branch edits README.
        let wt = add_worktree_on_branch(&root, &wt_base, "child-sides", "fanout/sides")
            .await
            .unwrap();
        std::fs::write(wt.join("README.md"), "branch-content\n").unwrap();
        git(&wt, &["add", "README.md"]);
        git(&wt, &["commit", "-m", "branch", "--quiet"]);
        remove_worktree(&root, &wt).await.unwrap();

        // Parent also edits README (different content).
        std::fs::write(root.join("README.md"), "parent-content\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-m", "parent", "--quiet"]);

        match merge_branch(&root, "fanout/sides").await.unwrap() {
            MergeAttempt::Conflicts { paths } => {
                assert!(paths.iter().any(|p| p.contains("README")));
                let sides = read_conflict_sides(&root, "README.md").await.unwrap();
                assert!(
                    sides.ours.contains("parent-content"),
                    "ours: {}",
                    sides.ours
                );
                assert!(
                    sides.theirs.contains("branch-content"),
                    "theirs: {}",
                    sides.theirs
                );
            }
            other => {
                // Abort and still check (merge might be clean in extremely rare cases).
                let _ = std::process::Command::new("git")
                    .args(["merge", "--abort"])
                    .current_dir(&root)
                    .status();
                panic!("expected conflicts, got {other:?}");
            }
        }
        let _ = std::process::Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(&root)
            .status();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn branch_tip_wraps_rev_parse() {
        let root = std::env::temp_dir().join(format!("lib-merge-tip-{}", unique()));
        init_repo(&root);
        let tip = branch_tip(&root, "HEAD").await;
        assert!(tip.is_ok(), "{tip:?}");
        let sha = tip.unwrap();
        assert!(!sha.is_empty());
        assert_eq!(sha.len(), 40); // full SHA hash
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn commit_merge_returns_real_sha() {
        let root = std::env::temp_dir().join(format!("lib-merge-cm2-{}", unique()));
        let wt_base = root.join("wts");
        init_repo(&root);

        // Create conflicting branch so commit_merge has something staged.
        let wt = add_worktree_on_branch(&root, &wt_base, "child-cm2", "fanout/cm2")
            .await
            .unwrap();
        std::fs::write(wt.join("README.md"), "branch\n").unwrap();
        git(&wt, &["add", "README.md"]);
        git(&wt, &["commit", "-m", "branch", "--quiet"]);
        remove_worktree(&root, &wt).await.unwrap();

        std::fs::write(root.join("README.md"), "parent\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-m", "parent", "--quiet"]);

        match merge_branch(&root, "fanout/cm2").await.unwrap() {
            MergeAttempt::Conflicts { .. } => {
                stage_resolution(&root, "README.md", "resolved\n")
                    .await
                    .unwrap();
                let sha = commit_merge(&root, "resolved").await.unwrap();
                assert!(!sha.is_empty());
                assert_eq!(sha.len(), 40, "commit SHA must be 40 hex chars: {sha}");
            }
            other => panic!("expected conflicts, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn safe_name_rejects_empty() {
        assert!(validate_safe_name("", "name").is_err());
    }

    #[test]
    fn safe_name_rejects_dot_dot() {
        assert!(validate_safe_name("..", "name").is_err());
        assert!(validate_safe_name("a../b", "name").is_err());
    }

    #[test]
    fn safe_name_rejects_slash() {
        assert!(validate_safe_name("a/b", "name").is_err());
    }

    #[test]
    fn safe_name_rejects_backslash() {
        assert!(validate_safe_name("a\\b", "name").is_err());
    }

    #[test]
    fn safe_name_rejects_dash_prefix() {
        assert!(validate_safe_name("-bad", "name").is_err());
    }

    #[test]
    fn safe_name_accepts_valid() {
        assert!(validate_safe_name("child-1", "name").is_ok());
        assert!(validate_safe_name("task_api", "name").is_ok());
    }

    #[test]
    fn branch_name_rejects_empty() {
        assert!(validate_branch_name("").is_err());
    }

    #[test]
    fn branch_name_rejects_dot_dot() {
        assert!(validate_branch_name("..").is_err());
    }

    #[test]
    fn branch_name_rejects_backslash() {
        assert!(validate_branch_name("a\\b").is_err());
    }

    #[test]
    fn branch_name_rejects_dash_prefix() {
        assert!(validate_branch_name("-evil").is_err());
    }

    #[test]
    fn branch_name_rejects_space() {
        assert!(validate_branch_name("bad name").is_err());
    }

    #[test]
    fn branch_name_rejects_empty_segment() {
        assert!(validate_branch_name("fanout//child").is_err());
        assert!(validate_branch_name("/child").is_err());
        assert!(validate_branch_name("fanout/").is_err());
    }

    #[test]
    fn branch_name_accepts_slash_delimited_paths() {
        assert!(validate_branch_name("fanout/child").is_ok());
        assert!(validate_branch_name("fanout/child/api").is_ok());
    }
}

/// Increments [`CONCURRENT_IN_REGISTRY`] on construction and decrements on drop, so a test can
/// assert the guarded section is never entered twice at once.
#[cfg(test)]
struct ConcurrencyProbe;

#[cfg(test)]
impl ConcurrencyProbe {
    fn enter() -> Self {
        CONCURRENT_IN_REGISTRY.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
mod registry_lock_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn seed_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().to_string_lossy().to_string();
        let git = |args: &[&str]| {
            let out = liberado_common::process::std_command("git")
                .args(args)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["-C", &p, "init", "-q"]);
        std::fs::write(
            dir.path().join("seed.txt"),
            "seed
",
        )
        .expect("seed");
        git(&["-C", &p, "add", "-A"]);
        git(&[
            "-C",
            &p,
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "seed",
        ]);
        dir
    }

    /// Four children setting up at once must all succeed, and must never be inside the registry
    /// section together.
    ///
    /// The count is the real assertion. "Run it concurrently and see if it passes" is how this
    /// race survived two encounters: it passes on Linux, passes locally on Windows, and fails
    /// about one run in ten on a Windows runner.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_worktree_creation_is_serialized_and_succeeds() {
        let repo = seed_repo();
        let base = repo.path().join("wt");
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut handles = Vec::new();
        for i in 0..4 {
            let root = repo.path().to_path_buf();
            let base = base.clone();
            let peak = std::sync::Arc::clone(&peak);
            handles.push(tokio::spawn(async move {
                let name = format!("child-{i}");
                let branch = format!("fanout/child-{i}");
                let result = add_worktree_on_branch(&root, &base, &name, &branch).await;
                let seen = CONCURRENT_IN_REGISTRY.load(Ordering::SeqCst);
                peak.fetch_max(seen, Ordering::SeqCst);
                result
            }));
        }

        for h in handles {
            h.await
                .expect("task")
                .expect("every child must get a worktree");
        }
        assert!(
            peak.load(Ordering::SeqCst) <= 1,
            "two tasks were inside the worktree registry at once; the lock is not held"
        );
    }
}
