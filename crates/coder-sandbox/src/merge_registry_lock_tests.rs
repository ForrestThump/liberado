//! Split from `merge.rs` for module-health boundaries.

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
