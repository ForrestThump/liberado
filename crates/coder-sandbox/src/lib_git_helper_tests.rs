//! Split from `lib.rs` for module-health boundaries.

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for args in [
        ["init", "--quiet"].as_slice(),
        ["config", "user.email", "test@liberado.local"].as_slice(),
        ["config", "user.name", "liberado-test"].as_slice(),
    ] {
        assert!(
            liberado_common::process::std_command(GIT)
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }
}

#[tokio::test]
async fn run_git_returns_stdout() {
    let dir = std::env::temp_dir().join(format!("lib-git-helper-{}", unique()));
    init_repo(&dir);
    std::fs::write(dir.join("test.txt"), "hello").unwrap();
    let status = run_git(&dir, &["status", "--porcelain"]).await.unwrap();
    assert!(
        status.contains("test.txt"),
        "expected test.txt in status: {status}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn run_git_errors_on_bad_command() {
    let dir = std::env::temp_dir().join(format!("lib-git-err-{}", unique()));
    init_repo(&dir);
    let err = run_git(&dir, &["this-is-not-a-git-subcommand"]).await;
    assert!(err.is_err(), "bad git command should fail");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn run_git_best_effort_does_not_panic_on_failure() {
    let dir = std::env::temp_dir().join(format!("lib-git-be-{}", unique()));
    init_repo(&dir);
    let _ = run_git_best_effort(&dir, &["this-is-not-a-git-subcommand"]).await;
    // Should not panic.
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_session_worktree_id_rejects_bad_ids() {
    assert!(validate_session_worktree_id("").is_err());
    assert!(validate_session_worktree_id("a/b").is_err());
    assert!(validate_session_worktree_id("a\\b").is_err());
    assert!(validate_session_worktree_id("..").is_err());
    assert!(validate_session_worktree_id("a../b").is_err());
}

#[test]
fn validate_session_worktree_id_accepts_good_ids() {
    assert!(validate_session_worktree_id("session-1").is_ok());
    assert!(validate_session_worktree_id("abc_def").is_ok());
    assert!(validate_session_worktree_id("task42").is_ok());
}
