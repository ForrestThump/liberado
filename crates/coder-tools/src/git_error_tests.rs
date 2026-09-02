//! Survivor tests for the git tool surface: error display, commit identity
//! and staging scope, transport-tool output contracts, and log formatting.
//!
//! Reuses the identity-free repo fixtures from `tool_survivor_tests` so the
//! tests stay honest on CI runners without a host git identity.

use super::tool_survivor_tests::{repo_without_user_config_for_tests, run_git_for_tests};
use super::*;

// ── error rendering ─────────────────────────────────────────────────────────

/// Display renders the message verbatim; it is what users see on failures.
#[test]
fn git_error_display_is_the_message() {
    let err = GitError {
        exit_code: 3,
        message: "boom happened".into(),
    };
    assert_eq!(err.to_string(), "boom happened");
}

// ── commit ──────────────────────────────────────────────────────────────────

/// Agent commits carry the liberado identity, readable through `--format`.
#[test]
fn agent_commits_use_the_liberado_identity() {
    let parent = tempfile::tempdir().unwrap();
    let root = repo_without_user_config_for_tests(&parent);
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    let out = commit(&root, "first", None).expect("commit succeeds");
    assert!(out.contains("first"), "{out}");

    let authors = run_git_tool(&root, &["log", "--format=%an|%ae"]).unwrap();
    assert_eq!(authors.trim(), "liberado|liberado@local");
}

/// Committing named files stages only those files: unrelated modifications
/// stay in the working tree.
#[test]
fn commit_with_file_list_stages_only_named_files() {
    let parent = tempfile::tempdir().unwrap();
    let root = repo_without_user_config_for_tests(&parent);
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    std::fs::write(root.join("b.txt"), "one\n").unwrap();
    commit(&root, "base", None).unwrap();

    std::fs::write(root.join("a.txt"), "two\n").unwrap();
    std::fs::write(root.join("b.txt"), "two\n").unwrap();
    commit(&root, "only a", Some(&["a.txt".to_string()])).unwrap();

    let status = status(&root).unwrap();
    assert!(
        status.contains("b.txt"),
        "b.txt must remain unstaged: {status:?}"
    );
    assert!(!status.contains("a.txt"), "a.txt was committed: {status:?}");
}

/// An empty pathspec is rejected by the tool with its own message, before
/// git ever sees it.
#[test]
fn empty_pathspec_is_rejected_by_the_tool() {
    let parent = tempfile::tempdir().unwrap();
    let root = repo_without_user_config_for_tests(&parent);
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    let err = commit(&root, "x", Some(&[String::new()])).unwrap_err();
    assert!(
        err.message.contains("invalid file path"),
        "tool-level rejection expected: {err}"
    );
}

// ── transport tools ─────────────────────────────────────────────────────────

fn push_to_bare_and_fetch_back() -> (tempfile::TempDir, std::path::PathBuf) {
    // Work repo with one commit.
    let work_parent = tempfile::tempdir().unwrap();
    let work = repo_without_user_config_for_tests(&work_parent);
    std::fs::write(work.join("s.txt"), "initial\n").unwrap();
    run_git_for_tests(&work, ["add", "-A"].as_slice());
    run_git_for_tests(&work, ["commit", "--quiet", "-m", "base"].as_slice());

    // Bare origin beside it (path stays valid for both closures).
    let bare = work_parent.path().join("origin.git");
    run_git_for_tests(
        work.as_path(),
        ["clone", "--quiet", "--bare", ".", bare.to_str().unwrap()].as_slice(),
    );

    // A second clone that can fetch from the same origin.
    let clone = work_parent.path().join("clone");
    run_git_for_tests(
        work.as_path(),
        [
            "clone",
            "--quiet",
            bare.to_str().unwrap(),
            clone.to_str().unwrap(),
        ]
        .as_slice(),
    );
    (work_parent, clone)
}

/// Pushing to a local bare remote succeeds quietly: progress goes to stderr,
/// stdout comes back empty.
#[test]
fn push_to_local_remote_keeps_stdout_quiet() {
    let (_parent, clone) = push_to_bare_and_fetch_back();
    std::fs::write(clone.join("s.txt"), "second\n").unwrap();
    run_git_for_tests(&clone, ["add", "-A"].as_slice());
    run_git_for_tests(&clone, ["commit", "--quiet", "-m", "next"].as_slice());

    let branch = run_git_tool(&clone, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let out = push(&clone, "origin", Some(&branch), false).unwrap();
    assert_eq!(out, "", "push progress belongs on stderr: {out:?}");
}

/// Fetching an up-to-date local remote also keeps stdout empty.
#[test]
fn fetch_from_local_remote_keeps_stdout_quiet() {
    let (_parent, clone) = push_to_bare_and_fetch_back();
    let out = fetch(&clone, "origin", None).unwrap();
    assert_eq!(out, "", "fetch progress belongs on stderr: {out:?}");
}

/// A fast-forward merge reports what it advanced past, on stdout.
#[test]
fn fast_forward_merge_reports_the_update() {
    let parent = tempfile::tempdir().unwrap();
    let root = repo_without_user_config_for_tests(&parent);
    std::fs::write(root.join("f.txt"), "one\n").unwrap();
    run_git_for_tests(&root, ["add", "-A"].as_slice());
    run_git_for_tests(&root, ["commit", "--quiet", "-m", "base"].as_slice());
    let default_branch = run_git_tool(&root, &["symbolic-ref", "--short", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    run_git_for_tests(&root, ["branch", "side"].as_slice());

    std::fs::write(root.join("f.txt"), "two\n").unwrap();
    run_git_for_tests(&root, ["add", "-A"].as_slice());
    run_git_for_tests(&root, ["commit", "--quiet", "-m", "ahead"].as_slice());

    // Merge the default branch into side from side's checkout: pure ff.
    run_git_for_tests(&root, ["checkout", "--quiet", "side"].as_slice());
    let out = merge(&root, &default_branch, true).unwrap();
    assert!(
        out.contains("Fast-forward") || out.starts_with("Updating"),
        "ff merge narrates on stdout: {out:?}"
    );
}

// ── log formatting ──────────────────────────────────────────────────────────

fn repo_with_one_commit() -> tempfile::TempDir {
    let parent = tempfile::tempdir().unwrap();
    let root = repo_without_user_config_for_tests(&parent);
    std::fs::write(root.join("a.txt"), "content\n").unwrap();
    run_git_for_tests(&root, ["add", "-A"].as_slice());
    run_git_for_tests(&root, ["commit", "--quiet", "-m", "the subject"].as_slice());
    parent
}

/// The default walk answers `%h %s`; a custom format is passed through
/// faithfully; an empty format falls back to the default walk too.
#[test]
fn log_format_routing() {
    let parent = repo_with_one_commit();
    let root = parent.path().join("repo");

    let default = log(&root, 5, None, None).unwrap();
    assert!(default.contains("the subject"), "{default:?}");

    let subject_only = log(&root, 5, Some("%s"), None).unwrap();
    assert_eq!(
        subject_only.trim(),
        "the subject",
        "custom format must reach git: {subject_only:?}"
    );

    let empty_format = log(&root, 5, Some(""), None).unwrap();
    assert!(
        !empty_format.trim().is_empty(),
        "empty format falls back to the default walk: {empty_format:?}"
    );
}

#[tokio::test]
async fn git_fetch_rejects_empty_branch() {
    let dir = tempfile::tempdir().unwrap();
    crate::tests::init_temp_git_repo(dir.path());
    let runtime = crate::CodingToolRuntime::new(
        dir.path(),
        liberado_coder_core::CommandPolicy::default(),
        liberado_coder_core::PathPolicy::default(),
    )
    .unwrap();
    let err = runtime
        .invoke_json(
            "git_fetch",
            serde_json::json!({"remote": "origin", "branch": ""}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("must not be empty"));
}

#[tokio::test]
async fn git_fetch_rejects_dash_prefixed_branch() {
    let dir = tempfile::tempdir().unwrap();
    crate::tests::init_temp_git_repo(dir.path());
    let runtime = crate::CodingToolRuntime::new(
        dir.path(),
        liberado_coder_core::CommandPolicy::default(),
        liberado_coder_core::PathPolicy::default(),
    )
    .unwrap();
    let err = runtime
        .invoke_json(
            "git_fetch",
            serde_json::json!({"remote": "origin", "branch": "--depth"}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("must not start with '-'"));
}

#[tokio::test]
async fn git_fetch_reports_a_missing_remote() {
    let dir = tempfile::tempdir().unwrap();
    crate::tests::init_temp_git_repo(dir.path());
    let runtime = crate::CodingToolRuntime::new(
        dir.path(),
        liberado_coder_core::CommandPolicy::default(),
        liberado_coder_core::PathPolicy::default(),
    )
    .unwrap();
    let result = runtime
        .invoke_json("git_fetch", serde_json::json!({"remote": "no-such-remote"}))
        .await
        .expect("a git failure is a result, not a tool error");
    assert_ne!(result["exit_code"], 0);
    assert!(!result["stderr"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn git_fetch_from_a_local_origin() {
    let dir = tempfile::tempdir().unwrap();
    crate::tests::init_temp_git_repo(dir.path());
    let bare = tempfile::tempdir().unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .current_dir(bare.path())
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let origin = bare.path().display().to_string().replace('\\', "/");
    let add = std::process::Command::new("git")
        .args(["remote", "add", "origin", &origin])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let push = std::process::Command::new("git")
        .args(["push", "--quiet", "origin", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        push.status.success(),
        "{}",
        String::from_utf8_lossy(&push.stderr)
    );
    let runtime = crate::CodingToolRuntime::new(
        dir.path(),
        liberado_coder_core::CommandPolicy::default(),
        liberado_coder_core::PathPolicy::default(),
    )
    .unwrap();
    let result = runtime
        .invoke_json("git_fetch", serde_json::json!({"remote": "origin"}))
        .await
        .expect("fetch from a local origin must be a result");
    assert_eq!(result["exit_code"], 0, "{result}");
}
