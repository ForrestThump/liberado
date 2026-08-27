//! Task-context assembly: what `build_task_context` feeds the model from the workspace summary
//! and prior-round session history.
//!
//! These live outside `main.rs` because that file sits over every module-health review
//! boundary — any line added there regresses the ratchet (see `module-health.toml`).

use super::{
    SessionRound, build_task_context, derive_task_id, now_unix_seconds, push_work,
    session_state_dir,
};
use liberado_coder_core::CoderTuning;
use liberado_common::process::std_command;

/// build_task_context with the repo map disabled: the workspace summary (and prior-round
/// history when a session id is given) is all that feeds the task context; no shell-out.
#[tokio::test]
async fn build_task_context_uses_workspace_summary_and_session_history() {
    let dir = tempfile::tempdir().unwrap();
    let tuning = CoderTuning::default();
    let (ctx, task_id) = build_task_context(
        "do the thing",
        dir.path(),
        &tuning,
        None,
        "Workspace contents:\n  (empty workspace)",
    )
    .await
    .unwrap();
    assert_eq!(task_id, derive_task_id(None, "do the thing"));
    let ctx = ctx.expect("the workspace summary must make the context non-empty");
    assert!(ctx.contains("Workspace contents:"), "{ctx}");
    assert!(!ctx.contains("Session history"), "{ctx}");
}

/// With a session id and a prior round on disk, the task context carries the history.
#[tokio::test]
async fn build_task_context_includes_prior_rounds() {
    let dir = tempfile::tempdir().unwrap();
    let state = session_state_dir(dir.path(), "sess-1");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("01.json"),
        serde_json::to_vec(&SessionRound {
            session_id: "sess-1".into(),
            round: 0,
            prompt: "first ask".into(),
            summary: "did it".into(),
            files_changed: vec!["a.txt".into()],
        })
        .unwrap(),
    )
    .unwrap();
    let tuning = CoderTuning::default();
    let (ctx, _) = build_task_context("second ask", dir.path(), &tuning, Some("sess-1"), "")
        .await
        .unwrap();
    let ctx = ctx.expect("the session history must make the context non-empty");
    assert!(ctx.contains("Session history"), "{ctx}");
    assert!(ctx.contains("first ask"), "{ctx}");
    assert!(ctx.contains("a.txt"), "{ctx}");
}

/// When both workspace summary and session history are present, they must be separated
/// by a double newline — not concatenated without a boundary.
#[tokio::test]
async fn build_task_context_separates_workspace_summary_from_session_history() {
    let dir = tempfile::tempdir().unwrap();
    let state = session_state_dir(dir.path(), "sess-2");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("01.json"),
        serde_json::to_vec(&SessionRound {
            session_id: "sess-2".into(),
            round: 0,
            prompt: "prior question".into(),
            summary: "did stuff".into(),
            files_changed: vec![],
        })
        .unwrap(),
    )
    .unwrap();
    let tuning = CoderTuning::default();
    let (ctx, _) = build_task_context(
        "new task",
        dir.path(),
        &tuning,
        Some("sess-2"),
        "Workspace:\n  src/main.rs",
    )
    .await
    .unwrap();
    let ctx = ctx.unwrap();
    // Both sections must appear...
    assert!(ctx.contains("Workspace:"), "{ctx}");
    assert!(ctx.contains("Session history"), "{ctx}");
    // ...and be separated by a blank line, not smashed together.
    let ws_end = ctx.find("src/main.rs").unwrap() + "src/main.rs".len();
    let session_start = ctx.find("Session history").unwrap();
    let between = &ctx[ws_end..session_start];
    assert!(
        between.contains("\n\n"),
        "workspace summary and session history must be separated by a blank line, got: {between:?}"
    );
}

/// Mutation-survivor kills (see `docs/validation/mutation-testing/mutation-testing-report-coder-runner.md`).
/// These live here rather than in a new file so `main.rs` stays under the module-health
/// ratchet — every line added to `main.rs` regresses its Ploc baseline.
/// A temp repo with one committed file, a bare `origin` remote, and a branch ready to push.
fn survivor_temp_repo_with_remote() -> (tempfile::TempDir, tempfile::TempDir) {
    let work = tempfile::tempdir().expect("tempdir");
    let wp = work.path().to_string_lossy().to_string();
    let bare = tempfile::tempdir().expect("bare tempdir");
    let bp = bare.path().to_string_lossy().to_string();

    let run = |args: &[&str]| {
        let out = std_command("git").args(args).output().expect("git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    run(&["-C", &wp, "init", "-q"]);
    run(&["-C", &bp, "init", "--bare", "-q"]);
    run(&["-C", &wp, "remote", "add", "origin", &bp]);
    std::fs::write(work.path().join("seed.txt"), "seed\n").expect("seed");
    run(&["-C", &wp, "add", "-A"]);
    run(&[
        "-C",
        &wp,
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "seed",
    ]);
    run(&["-C", &wp, "checkout", "-q", "-b", "agent/x-123"]);
    (work, bare)
}

#[test]
fn now_unix_seconds_is_a_recent_epoch() {
    // Kills the mutants that replace the body with `0` or `1`: a real clock reads far above the
    // 2023 epoch, so any constant collapses to a wrong, tiny value.
    let t = now_unix_seconds();
    assert!(
        t > 1_700_000_000,
        "now_unix_seconds returned {t}; expected a recent unix timestamp"
    );
}

#[tokio::test]
async fn push_work_pushes_the_branch_to_origin() {
    let (work, bare) = survivor_temp_repo_with_remote();
    push_work(work.path(), "agent/x-123").await;

    let out = std_command("git")
        .args(["ls-remote", "--heads", bare.path().to_str().unwrap()])
        .output()
        .expect("ls-remote");
    let remote = String::from_utf8_lossy(&out.stdout);
    assert!(
        remote.contains("agent/x-123"),
        "push_work did not push the branch to origin: {remote}"
    );
}
