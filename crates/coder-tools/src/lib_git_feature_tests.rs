//! Git feature-on/off tests split from `lib.rs` for module-health.
//!
//! Covers dedicated git tools under `--features git` and the stub path when
//! that feature is off. Helpers are duplicated here so the inline `mod tests`
//! can keep the ungated git argument-validation cases.

use super::*;
use liberado_coder_core::{CommandPolicy, PathPolicy};
use serde_json::json;

pub(crate) fn init_temp_git_repo(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "--quiet"]);
    // Repo-local identity: exists on every dev machine and on no CI runner
    // unless we write it. `commit.gpgsign=false` for the reverse case.
    run(&["config", "user.email", "test@liberado.local"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("seed.txt"), "initial\n").unwrap();
    run(&["add", "seed.txt"]);
    run(&["commit", "-m", "initial commit"]);
}

/// A git repo with one committed file, for the untracked-diff tests.
///
/// `user.email` / `user.name` are set explicitly: they exist on every dev machine and on no
/// CI runner, so a `git commit` that relies on ambient identity passes locally and fails in
/// CI. `commit.gpgsign=false` for the same reason in reverse — a developer who signs by
/// default would otherwise block on a key the runner does not have.
fn git_repo_with_one_committed_file() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("git available");
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    };
    run(&["init", "--quiet"]);
    run(&["config", "user.email", "test@liberado.local"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("tracked.txt"), "one\n").unwrap();
    run(&["add", "tracked.txt"]);
    run(&["commit", "--quiet", "-m", "base"]);
    dir
}

#[cfg(feature = "git")]
fn git_add_commit(dir: &std::path::Path, message: &str) {
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .unwrap();
}

/// C1's other half: the dedicated git tools still work with the default policy — they go
/// through the gix-backed path, not `run_command`.
#[cfg(feature = "git")]
#[tokio::test]
async fn dedicated_git_tool_still_works_with_default_policy() {
    let dir = tempfile::tempdir().unwrap();
    init_temp_git_repo(dir.path());
    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();

    let result = runtime
        .invoke_json("git_branch", json!({"name": "c1-test-branch"}))
        .await
        .unwrap();
    assert_eq!(result["branch"], "c1-test-branch");
    assert_eq!(result["exit_code"], 0);

    let current = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let branch = String::from_utf8_lossy(&current.stdout).trim().to_string();
    assert_eq!(branch, "c1-test-branch");
}

/// The bug this closes, in the form it actually took.
///
/// A run wrote a new module, then called `git_diff` four times and was shown nothing each
/// time, because `git diff` reports tracked files only. The model concluded the file had
/// never been written and wrote all 334 lines again.
#[cfg(feature = "git")]
#[tokio::test]
async fn a_new_file_appears_in_the_diff() {
    let dir = git_repo_with_one_committed_file();
    std::fs::write(dir.path().join("brand_new.rs"), "fn hello() {}\n").unwrap();
    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();

    for mode in ["name_only", "stat", "patch"] {
        let result = runtime
            .invoke_json("git_diff", json!({ "mode": mode }))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap_or_default();
        assert!(
            stdout.contains("brand_new.rs"),
            "mode {mode} must show a newly created file, got: {stdout:?}"
        );
    }
}

/// Names were what the critic already had, and they were not enough to review a change.
#[cfg(feature = "git")]
#[tokio::test]
async fn patch_mode_carries_the_new_file_content() {
    let dir = git_repo_with_one_committed_file();
    std::fs::write(dir.path().join("brand_new.rs"), "fn hello() {}\n").unwrap();
    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();

    let result = runtime
        .invoke_json("git_diff", json!({"mode": "patch"}))
        .await
        .unwrap();
    let stdout = result["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.contains("+fn hello() {}"),
        "patch mode must carry the content, not only the name: {stdout:?}"
    );
}

/// Tracked edits must survive the addition. Appending the untracked section is worthless if
/// it displaces the answer the tool already gave.
#[cfg(feature = "git")]
#[tokio::test]
async fn tracked_changes_still_appear_alongside_untracked_ones() {
    let dir = git_repo_with_one_committed_file();
    std::fs::write(dir.path().join("tracked.txt"), "one\ntwo\n").unwrap();
    std::fs::write(dir.path().join("brand_new.rs"), "fn hello() {}\n").unwrap();
    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();

    let result = runtime
        .invoke_json("git_diff", json!({"mode": "name_only"}))
        .await
        .unwrap();
    let stdout = result["stdout"].as_str().unwrap_or_default();
    assert!(stdout.contains("tracked.txt"), "{stdout:?}");
    assert!(stdout.contains("brand_new.rs"), "{stdout:?}");
}

#[cfg(feature = "git")]
#[tokio::test]
async fn git_branch_creates_and_switches() {
    let dir = tempfile::tempdir().unwrap();
    init_temp_git_repo(dir.path());
    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();

    let result = runtime
        .invoke_json("git_branch", json!({"name": "feature-x"}))
        .await
        .unwrap();
    assert_eq!(result["branch"], "feature-x");
    assert_eq!(result["exit_code"], 0);

    let current = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let branch = String::from_utf8_lossy(&current.stdout).trim().to_string();
    assert_eq!(branch, "feature-x");
}

#[cfg(feature = "git")]
#[tokio::test]
async fn git_commit_stages_and_commits() {
    let dir = tempfile::tempdir().unwrap();
    init_temp_git_repo(dir.path());
    std::fs::write(dir.path().join("new_file.txt"), "content\n").unwrap();

    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();

    let result = runtime
        .invoke_json(
            "git_commit",
            json!({"message": "add new file", "files": ["new_file.txt"]}),
        )
        .await
        .unwrap();
    assert_eq!(result["committed"], true);
    assert_eq!(result["exit_code"], 0);

    let log = std::process::Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let log_text = String::from_utf8_lossy(&log.stdout);
    assert!(log_text.contains("add new file"));
}

#[cfg(feature = "git")]
#[tokio::test]
async fn git_commit_stages_all_when_no_files_given() {
    let dir = tempfile::tempdir().unwrap();
    init_temp_git_repo(dir.path());
    std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();

    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();

    let result = runtime
        .invoke_json("git_commit", json!({"message": "commit all"}))
        .await
        .unwrap();
    assert_eq!(result["committed"], true);
    assert_eq!(result["exit_code"], 0);

    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&status.stdout).trim().is_empty());
}

#[cfg(feature = "git")]
#[tokio::test]
async fn git_log_returns_recent_commits() {
    let dir = tempfile::tempdir().unwrap();
    init_temp_git_repo(dir.path());
    std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
    git_add_commit(dir.path(), "second commit");

    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();
    let result = runtime.invoke_json("git_log", json!({})).await.unwrap();
    assert_eq!(result["exit_code"], 0);
    let stdout = result["stdout"].as_str().unwrap();
    assert!(stdout.contains("second commit"));
    assert!(stdout.contains("initial commit"));
}

#[cfg(feature = "git")]
#[tokio::test]
async fn git_log_respects_limit_and_branch() {
    let dir = tempfile::tempdir().unwrap();
    init_temp_git_repo(dir.path());
    for i in 0..5 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
        git_add_commit(dir.path(), &format!("commit {i}"));
    }

    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();
    let result = runtime
        .invoke_json("git_log", json!({"limit": 2}))
        .await
        .unwrap();
    let stdout = result["stdout"].as_str().unwrap();
    let count = stdout.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(count, 2);
}

#[cfg(feature = "git")]
#[tokio::test]
async fn git_diff_stat_and_patch_modes() {
    let dir = tempfile::tempdir().unwrap();
    init_temp_git_repo(dir.path());
    // seed.txt already exists from init, modify it so git diff has something to show
    std::fs::write(dir.path().join("seed.txt"), "modified content\n").unwrap();

    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();

    let stat = runtime
        .invoke_json("git_diff", json!({"mode": "stat"}))
        .await
        .unwrap();
    assert!(stat["stdout"].as_str().unwrap().contains("seed.txt"));

    let patch = runtime
        .invoke_json("git_diff", json!({"mode": "patch"}))
        .await
        .unwrap();
    assert!(patch["stdout"].as_str().unwrap().contains("@@"));
}

/// Without the `git` feature, dedicated git tools return a clear unavailable error
/// that points operators at `coder-full` / `liberado-coder-run` rather than only
/// `--features git`.
#[cfg(not(feature = "git"))]
#[tokio::test]
async fn git_tools_stub_mention_coder_full_when_feature_off() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();

    let status = runtime.invoke_json("git_status", json!({})).await.unwrap();
    assert_eq!(status["exit_code"], 1);
    let stderr = status["stderr"].as_str().unwrap_or_default();
    assert!(
        stderr.contains("coder-full") && stderr.contains("liberado-coder-run"),
        "git_status stub must mention coder-full and liberado-coder-run: {stderr}"
    );

    let branch = runtime
        .invoke_json("git_branch", json!({"name": "feature-x"}))
        .await
        .unwrap();
    assert_eq!(branch["exit_code"], 1);
    let stderr = branch["stderr"].as_str().unwrap_or_default();
    assert!(
        stderr.contains("unavailable") && stderr.contains("coder-full"),
        "git_branch stub must mention unavailable + coder-full: {stderr}"
    );

    let commit = runtime
        .invoke_json("git_commit", json!({"message": "x"}))
        .await
        .unwrap();
    assert_eq!(commit["committed"], false);
    let stderr = commit["stderr"].as_str().unwrap_or_default();
    assert!(
        stderr.contains("liberado-coder-run") || stderr.contains("liberado-coder-runner"),
        "git_commit stub must mention runner path: {stderr}"
    );
}
