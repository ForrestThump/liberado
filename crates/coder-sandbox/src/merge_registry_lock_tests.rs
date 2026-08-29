//! Split from `merge.rs` for module-health boundaries.

use super::*;
use crate::worktree_registry::PEAK_IN_REGISTRY;
use std::sync::atomic::Ordering;

fn reset_registry_peak() {
    PEAK_IN_REGISTRY.store(0, Ordering::SeqCst);
}

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
    reset_registry_peak();
    let repo = seed_repo();
    let base = repo.path().join("wt");

    let mut handles = Vec::new();
    for i in 0..4 {
        let root = repo.path().to_path_buf();
        let base = base.clone();
        handles.push(tokio::spawn(async move {
            let name = format!("child-{i}");
            let branch = format!("fanout/child-{i}");
            add_worktree_on_branch(&root, &base, &name, &branch).await
        }));
    }

    for h in handles {
        h.await
            .expect("task")
            .expect("every child must get a worktree");
    }
    assert!(
        PEAK_IN_REGISTRY.load(Ordering::SeqCst) <= 1,
        "two tasks were inside the worktree registry at once; the lock is not held"
    );
}

/// `remove_worktree` used to skip [`WORKTREE_REGISTRY`]. Its fallback prune +
/// `remove_dir_all` then raced a sibling `add` — including across parallel
/// tests that share a dest base. Peak > 1 means the remove path is unlocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_add_and_remove_are_serialized_and_succeed() {
    reset_registry_peak();
    let repos: Vec<_> = (0..4).map(|_| seed_repo()).collect();

    let mut handles = Vec::new();
    for (i, repo) in repos.iter().enumerate() {
        let root = repo.path().to_path_buf();
        let base = root.join("wt");
        handles.push(tokio::spawn(async move {
            let name = format!("child-{i}");
            let branch = format!("fanout/child-{i}");
            let wt = add_worktree_on_branch(&root, &base, &name, &branch).await?;
            remove_worktree(&root, &wt).await?;
            Ok::<_, MergeError>(())
        }));
    }

    for h in handles {
        h.await
            .expect("task")
            .expect("every child must add and remove");
    }
    assert!(
        PEAK_IN_REGISTRY.load(Ordering::SeqCst) <= 1,
        "add and remove overlapped in the registry; remove is not locked"
    );
}

/// Fan-out helpers are not the only worktree users. Durable and ephemeral workspaces use
/// `create_linked_worktree`; they must share the same registry guard with fan-out add/remove.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fanout_and_workspace_paths_share_one_registry_guard() {
    reset_registry_peak();
    let repos: Vec<_> = (0..8).map(|_| seed_repo()).collect();

    let mut handles = Vec::new();
    for (i, repo) in repos.iter().enumerate() {
        let root = repo.path().to_path_buf();
        handles.push(tokio::spawn(async move {
            if i % 2 == 0 {
                let base = root.join("fanout-worktrees");
                let name = format!("child-{i}");
                let branch = format!("fanout/child-{i}");
                let wt = add_worktree_on_branch(&root, &base, &name, &branch)
                    .await
                    .map_err(|e| e.to_string())?;
                remove_worktree(&root, &wt)
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                let mut workspace = crate::WorktreeWorkspace::new(
                    &root,
                    &format!("session-{i}"),
                    &root.join("session-worktrees"),
                    liberado_coder_core::CommandPolicy::default(),
                )
                .await
                .map_err(|e| e.to_string())?;
                workspace.cleanup().await;
            }
            Ok::<_, String>(())
        }));
    }

    for handle in handles {
        handle
            .await
            .expect("task")
            .expect("every worktree path must succeed");
    }
    assert!(
        PEAK_IN_REGISTRY.load(Ordering::SeqCst) <= 1,
        "fan-out and workspace paths overlapped in the shared registry"
    );
}

#[test]
fn worktree_add_retries_only_transient_git_failures() {
    assert!(worktree_add_is_retryable(
        "fatal: Unable to create '/tmp/repo/.git/index.lock': File exists"
    ));
    assert!(worktree_add_is_retryable(
        "'/tmp/wt/shared-child' already exists"
    ));
    assert!(worktree_add_is_retryable(
        "fatal: failed to read .git/worktrees/fanout-api-0/commondir: No error"
    ));
    assert!(worktree_add_is_retryable(
        "fatal: 'fanout/alpha-0' is already used by worktree at '/tmp/wt'"
    ));
    assert!(
        !worktree_add_is_retryable("fatal: not a git repository"),
        "a permanent setup error must not be retried"
    );
}

/// A leftover *file* at dest survives the first `remove_dir_all` and makes
/// `git worktree add` say "already exists". The retry arm must clear the file
/// and succeed — that is the mutation-site retry, not a test-level loop.
#[tokio::test]
async fn worktree_add_retries_when_dest_is_a_leftover_file() {
    let repo = seed_repo();
    let base = repo.path().join("wt");
    std::fs::create_dir_all(&base).expect("wt base");
    std::fs::write(base.join("file-dest"), "not a directory").expect("leftover file");

    let wt = add_worktree_on_branch(repo.path(), &base, "file-dest", "fanout/file-dest")
        .await
        .expect("retry must turn a file dest into a worktree");
    assert!(wt.join(".git").exists(), "linked worktree must be usable");
    remove_worktree(repo.path(), &wt).await.unwrap();
}
