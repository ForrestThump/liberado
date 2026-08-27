//! Task-context assembly: what `build_task_context` feeds the model from the workspace summary
//! and prior-round session history.
//!
//! These live outside `main.rs` because that file sits over every module-health review
//! boundary — any line added there regresses the ratchet (see `module-health.toml`).

use super::{SessionRound, build_task_context, derive_task_id, session_state_dir};
use liberado_coder_core::CoderTuning;

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
