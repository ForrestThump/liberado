//! Split from `coding_run.rs` for module-health boundaries.

use super::*;
/// Process-global env mutations in this crate must not race other tests.
///
/// `tokio::sync::Mutex`, not `std::sync::Mutex`: these tests hold the guard across an `.await`,
/// which `clippy::await_holding_lock` rejects for a blocking lock — it parks the whole runtime
/// thread rather than yielding. `coder-agent`'s `DATA_DIR_ENV_LOCK` is the same pattern for the
/// same reason. (Test binaries are per-crate, so this cannot be the *same* lock, only the same
/// shape.)
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A git repo with one commit, built without relying on any global git identity.
///
/// The identity is passed with `-c` here for the same reason `preserve_worktree` does it:
/// `user.email` / `user.name` exist on every dev machine and on no CI runner, so a fixture
/// that leans on global config passes locally and fails in CI.
fn temp_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().to_string_lossy().to_string();
    let run = |args: &[&str]| {
        let out = liberado_common::process::std_command("git")
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["-C", &p, "init", "-q"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("seed");
    run(&["-C", &p, "add", "-A"]);
    run(&[
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

fn is_dirty(repo: &std::path::Path) -> bool {
    let out = liberado_common::process::std_command("git")
        .args(["-C", &repo.to_string_lossy(), "status", "--porcelain"])
        .output()
        .expect("git status");
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

/// The whole point: a run's output must survive without anyone remembering to commit it.
///
/// Runs with `GIT_CONFIG_GLOBAL` pointed at an empty file, which is the CI condition — no
/// `user.name`, no `user.email`. Without the `-c` flags in `preserve_worktree` this fails
/// with "Please tell me who you are", which is precisely the failure that passes on a
/// developer box and breaks on a runner.
#[tokio::test]
async fn a_dirty_worktree_is_committed_even_with_no_global_git_identity() {
    let _guard = ENV_LOCK.lock().await;
    let repo = temp_repo();
    std::fs::write(repo.path().join("work.txt"), "agent output\n").expect("write");
    assert!(is_dirty(repo.path()), "precondition: tree must be dirty");

    let empty_cfg = tempfile::NamedTempFile::new().expect("cfg");
    // SAFETY: single-threaded under ENV_LOCK; removed below.
    unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", empty_cfg.path()) };

    let result = preserve_worktree(repo.path(), "done").await;

    unsafe { std::env::remove_var("GIT_CONFIG_GLOBAL") };

    let sha = result
        .expect("preserving a dirty worktree must succeed without global identity")
        .expect("a dirty tree must produce a commit");
    assert!(!sha.is_empty(), "commit sha must be reported");
    assert!(
        !is_dirty(repo.path()),
        "the tree must be clean after preservation - nothing left to lose"
    );
}

/// A clean tree must not manufacture an empty commit, or every prompt adds noise to history.
#[tokio::test]
async fn a_clean_worktree_produces_no_commit() {
    let _guard = ENV_LOCK.lock().await;
    let repo = temp_repo();
    assert!(!is_dirty(repo.path()), "precondition: tree must be clean");

    let preserved = preserve_worktree(repo.path(), "done")
        .await
        .expect("a clean tree is not an error");
    assert!(
        preserved.is_none(),
        "a clean tree must report nothing preserved, got {preserved:?}"
    );
}

#[tokio::test]
async fn prepare_workspace_fails_hard_when_worktree_setup_fails() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().expect("tempdir");
    // Looks like a git repo to is_git_repo, but is not a real repo → worktree create fails.
    std::fs::create_dir(dir.path().join(".git")).expect(".git");

    let data = tempfile::tempdir().expect("data dir");
    // SAFETY: single-threaded under env_lock; restored below.
    unsafe {
        std::env::set_var("LIBERADO_DATA_DIR", data.path());
    }

    let err = prepare_workspace(dir.path(), "sess-hard-fail")
        .await
        .expect_err("must not fall back to host cwd");
    assert!(
        err.contains("durable session worktree"),
        "error should name worktree setup, got: {err}"
    );
    assert!(
        err.contains("no live-tree fallback"),
        "error should refuse live-tree demotion, got: {err}"
    );

    unsafe {
        std::env::remove_var("LIBERADO_DATA_DIR");
    }
}

#[tokio::test]
async fn prepare_workspace_non_git_uses_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = prepare_workspace(dir.path(), "sess-nongit")
        .await
        .expect("non-git host cwd is ok");
    assert_eq!(path, dir.path());
}
