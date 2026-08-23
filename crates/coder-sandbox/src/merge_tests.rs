//! Split from `merge.rs` for module-health boundaries.

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn git(dir: &Path, args: &[&str]) {
    let ok = liberado_common::process::std_command("git")
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
async fn remove_worktree_deletes_the_directory_and_clears_the_registration() {
    let root = std::env::temp_dir().join(format!("lib-rmwt-ok-{}", unique()));
    let wt_base = root.join("wts");
    init_repo(&root);

    let wt = add_worktree_on_branch(&root, &wt_base, "child-b", "fanout/b")
        .await
        .unwrap();
    assert!(wt.exists());
    remove_worktree(&root, &wt).await.unwrap();
    assert!(!wt.exists(), "the worktree directory must be gone");
    let list = liberado_common::process::std_command("git")
        .args(["-C"])
        .arg(&root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        !stdout.contains("child-b"),
        "registration must be pruned, got: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A *locked* worktree makes `git worktree remove --force` refuse; the fallback must
/// still delete the directory and prune, or failed attempts leak worktrees forever.
#[cfg(unix)]
#[tokio::test]
async fn remove_worktree_falls_back_when_git_refuses() {
    let root = std::env::temp_dir().join(format!("lib-rmwt-lock-{}", unique()));
    let wt_base = root.join("wts");
    init_repo(&root);

    let wt = add_worktree_on_branch(&root, &wt_base, "child-c", "fanout/c")
        .await
        .unwrap();
    git(&wt, &["worktree", "lock", "."]);

    remove_worktree(&root, &wt).await.unwrap();
    assert!(
        !wt.exists(),
        "fallback must delete the directory when git refuses"
    );
    // Note: `git worktree prune` deliberately keeps a *locked* worktree's registration
    // even after its directory disappears, so only the directory is asserted here.
    // Unlocking first is the operator's escape hatch, not this function's job.
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
            let _ = liberado_common::process::std_command("git")
                .args(["merge", "--abort"])
                .current_dir(&root)
                .status();
            panic!("expected conflicts, got {other:?}");
        }
    }
    let _ = liberado_common::process::std_command("git")
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
