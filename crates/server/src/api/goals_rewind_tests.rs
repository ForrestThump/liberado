//! Split from `goals.rs` for module-health boundaries.

use super::*;
use liberado_session::{SessionEvent, SessionEventKind};

fn checkpoint(id: &str, label: &str, hash: &str) -> SessionEvent {
    SessionEvent {
        session_id: "s1".into(),
        at: chrono::Utc::now(),
        kind: SessionEventKind::Checkpoint {
            id: id.into(),
            label: label.into(),
            tree_hash: hash.into(),
        },
    }
}

fn token_event() -> SessionEvent {
    SessionEvent {
        session_id: "s1".into(),
        at: chrono::Utc::now(),
        kind: SessionEventKind::Token { text: "hi".into() },
    }
}

#[test]
fn rewind_workspace_prefers_an_existing_durable_dir() {
    let durable = tempfile::tempdir().unwrap();
    let got = rewind_workspace(Some(durable.path().to_path_buf()), Some("/payload")).unwrap();
    assert_eq!(got, durable.path());
}

#[test]
fn rewind_workspace_falls_back_to_payload_when_durable_is_missing() {
    let gone = std::path::PathBuf::from("C:\\definitely-not-here-rewind-test");
    let got = rewind_workspace(Some(gone), Some("/payload")).unwrap();
    assert_eq!(got, std::path::PathBuf::from("/payload"));
}

#[test]
fn rewind_workspace_uses_payload_when_no_durable_dir_exists() {
    let got = rewind_workspace(None, Some("/payload")).unwrap();
    assert_eq!(got, std::path::PathBuf::from("/payload"));
}

#[test]
fn rewind_workspace_reports_both_missing_sources() {
    let err = rewind_workspace(None, None).unwrap_err();
    assert!(err.contains("no workspace_root in payload"), "{err}");
    let gone = std::path::PathBuf::from("C:\\definitely-not-here-rewind-test");
    let err = rewind_workspace(Some(gone), None).unwrap_err();
    assert!(err.contains("no durable session worktree"), "{err}");
}

#[test]
fn rewind_checkpoint_explicit_id_wins_with_event_label() {
    let events = vec![
        checkpoint("c1", "first", "h1"),
        token_event(),
        checkpoint("c2", "second", "h2"),
    ];
    let got = rewind_checkpoint(&events, Some("c1")).unwrap();
    assert_eq!(got, ("c1".into(), "first".into(), "h1".into()));
}

#[test]
fn rewind_checkpoint_unknown_explicit_id_falls_back_to_explicit_label() {
    let events = vec![checkpoint("c1", "first", "h1")];
    let got = rewind_checkpoint(&events, Some("nope")).unwrap();
    assert_eq!(got, ("nope".into(), "explicit".into(), String::new()));
}

#[test]
fn rewind_checkpoint_no_id_uses_the_most_recent_checkpoint() {
    let events = vec![
        checkpoint("c1", "first", "h1"),
        token_event(),
        checkpoint("c2", "second", "h2"),
    ];
    let got = rewind_checkpoint(&events, None).unwrap();
    assert_eq!(got, ("c2".into(), "second".into(), "h2".into()));
}

#[test]
fn rewind_checkpoint_no_checkpoints_errors() {
    let err = rewind_checkpoint(&[token_event()], None).unwrap_err();
    assert!(err.contains("no checkpoint events"), "{err}");
}

// ── the HTTP handler itself ─────────────────────────────────────────────────────────

use axum::{Router, body::Body, http::Request};
use liberado_common::{CapabilityCatalog, CapabilitySet};
use liberado_session::{GoalSessionHub, GoalSessionStore, SessionRecordStore};
use std::time::Instant;
use tower::ServiceExt;

/// A coding-domain pack that finishes instantly: rewind only needs the session to exist with
/// coding domain and events, not a live build.
struct InstantCodingPack;

#[async_trait::async_trait]
impl liberado_session::DomainPackRunner for InstantCodingPack {
    fn domain_id(&self) -> &str {
        liberado_session::CODING_DOMAIN
    }

    async fn run(
        &self,
        _session_id: &str,
        _goal: &liberado_session::GoalSpec,
        _ctx: &liberado_session::PackContext<'_>,
        _events: tokio::sync::mpsc::Sender<liberado_session::SessionEvent>,
        _inputs: liberado_session::InputChannel,
        _cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<liberado_session::GoalResult, liberado_session::PackError> {
        Ok(liberado_session::GoalResult {
            terminal: liberado_session::TerminalKind::Succeeded,
            summary: "instant".into(),
            artifacts: vec![],
            diagnostics: serde_json::Value::Null,
        })
    }
}

use crate::boot_helper_tests::ENV_LOCK as DATA_DIR_ENV_LOCK;

/// Restores `LIBERADO_DATA_DIR` on drop. Declare after `DATA_DIR_ENV_LOCK` so Drop runs while
/// the lock is still held (reverse declaration order) — same pattern as `lib_boot_helper_tests`.
struct RestoreDataDir {
    prior: Option<std::ffi::OsString>,
}

impl RestoreDataDir {
    fn set_to(path: impl AsRef<std::ffi::OsStr>) -> Self {
        let prior = std::env::var_os("LIBERADO_DATA_DIR");
        // SAFETY: caller holds `DATA_DIR_ENV_LOCK` for the life of this guard.
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", path);
        }
        Self { prior }
    }
}

impl Drop for RestoreDataDir {
    fn drop(&mut self) {
        // SAFETY: same lock the constructor required; Drop runs before `_env` is released.
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var("LIBERADO_DATA_DIR", v),
                None => std::env::remove_var("LIBERADO_DATA_DIR"),
            }
        }
    }
}

fn rewind_app() -> (Router, Arc<GoalSessionHub>, Arc<GoalSessionStore>) {
    let store = Arc::new(GoalSessionStore::new());
    let mut hub = GoalSessionHub::new((*store).clone());
    hub.register_pack(Arc::new(InstantCodingPack));
    hub.register_pack(Arc::new(liberado_session::LifeOpsDemoRunner));
    let goals = Arc::new(hub);

    let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(AppState {
        start_time: Instant::now(),
        reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        dispatcher_attached: false,
        orchestrator_attached: false,
        watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vault_path: "/tmp/vault".to_string(),
        goals: goals.clone(),
        chat: None,
        chat_tools: 0,
        chat_tool_names: Vec::new(),
        catalog: Arc::new(CapabilityCatalog::new()),
        data_dir: std::path::PathBuf::from("/tmp/liberado"),
        sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
        main_agent_capabilities: CapabilitySet::empty(),
        dispatcher_capabilities: CapabilitySet::empty(),
        config: Arc::new(liberado_bootstrap::Config::default()),
        sessions: Arc::new(Default::default()),
        model_name: None,
        provider: None,
        hooks: std::collections::HashMap::new(),
        hook_tx,
        hook_idempotency: crate::hooks::IdempotencyCache::default(),
        live_mcp: liberado_bootstrap::LiveMcpController::empty(),
        drain: crate::shutdown::DrainGate::default(),
    });

    let app = Router::new()
        .route("/api/goals/{id}/rewind", axum::routing::post(goals_rewind))
        .route(
            "/api/goals/{id}/diff",
            axum::routing::get(super::goals_diff),
        )
        .with_state(state);
    (app, goals, store)
}

async fn post_rewind(
    app: &Router,
    id: &str,
    checkpoint: Option<&str>,
) -> axum::http::Response<Body> {
    let uri = format!("/api/goals/{id}/rewind");
    // Always a JSON body: an empty body under a JSON content-type is rejected by the
    // extractor before the handler runs, which is not a path this endpoint owns.
    let body = match checkpoint {
        Some(cp) => Body::from(format!(r#"{{"checkpoint_id":"{cp}"}}"#)),
        None => Body::from("{}"),
    };
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn start_coding_session(goals: &Arc<GoalSessionHub>, payload: serde_json::Value) -> String {
    use liberado_session::{DomainHint, GoalSpec};
    goals
        .start(GoalSpec {
            id: None,
            description: "rewind me".into(),
            success_criteria: vec![],
            domain: DomainHint::Coding,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload,
        })
        .await
        .expect("coding pack is registered")
}

#[tokio::test]
async fn rewind_of_an_unknown_session_is_404() {
    let (app, _goals, _store) = rewind_app();
    let response = post_rewind(&app, "g_ghost", None).await;
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    assert!(bytes.windows(5).any(|w| w == b"ghost"), "{:?}", bytes);
}

#[tokio::test]
async fn rewind_of_a_non_coding_session_is_400() {
    let (app, goals, _store) = rewind_app();
    let id = goals
        .start(liberado_session::GoalSpec {
            id: None,
            description: "life goal".into(),
            success_criteria: vec![],
            domain: liberado_session::DomainHint::Life,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::json!({}),
        })
        .await
        .expect("life demo pack is registered");
    let response = post_rewind(&app, &id, None).await;
    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("only supported for coding"), "{text}");
}

#[tokio::test]
async fn rewind_without_any_workspace_names_the_problem_as_400() {
    let (app, goals, _store) = rewind_app();
    let id = start_coding_session(&goals, serde_json::json!({})).await;
    let response = post_rewind(&app, &id, None).await;
    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("no workspace_root") || text.contains("no durable session worktree"),
        "{text}"
    );
}

#[tokio::test]
async fn rewind_with_a_checkpoint_but_no_real_workspace_fails_at_shadow_git_open_500() {
    let (app, goals, store) = rewind_app();
    let id = start_coding_session(
        &goals,
        serde_json::json!({ "workspace_root": "/nonexistent/rewind-test-ws" }),
    )
    .await;
    SessionRecordStore::push_event(
        store.as_ref(),
        SessionEvent::new(
            &id,
            SessionEventKind::Checkpoint {
                id: "cp-1".into(),
                label: "attempt-0-post".into(),
                tree_hash: "t1".into(),
            },
        ),
    )
    .await;

    let response = post_rewind(&app, &id, Some("cp-1")).await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("open shadow-git"), "{text}");
}

#[tokio::test]
async fn rewind_with_an_unknown_checkpoint_id_fails_the_restore_500() {
    let _env = DATA_DIR_ENV_LOCK.lock().await;
    let data = tempfile::tempdir().unwrap();
    let _restore_env = RestoreDataDir::set_to(data.path());

    let (app, goals, store) = rewind_app();
    let workspace = tempfile::tempdir().unwrap();
    let id = start_coding_session(
        &goals,
        serde_json::json!({
            "workspace_root": workspace.path().to_string_lossy(),
        }),
    )
    .await;
    SessionRecordStore::push_event(
        store.as_ref(),
        SessionEvent::new(
            &id,
            SessionEventKind::Checkpoint {
                id: "deadbeef".into(),
                label: "attempt-0-post".into(),
                tree_hash: "t1".into(),
            },
        ),
    )
    .await;

    let response = post_rewind(&app, &id, Some("deadbeef")).await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("restore checkpoint"), "{text}");
}

#[tokio::test]
async fn rewind_restores_the_latest_checkpoint_and_reports_it() {
    let _env = DATA_DIR_ENV_LOCK.lock().await;
    let data = tempfile::tempdir().unwrap();
    let _restore_env = RestoreDataDir::set_to(data.path());

    let (app, goals, store) = rewind_app();
    let workspace = tempfile::tempdir().unwrap();
    let id = start_coding_session(
        &goals,
        serde_json::json!({
            "workspace_root": workspace.path().to_string_lossy(),
        }),
    )
    .await;

    // A real snapshot in a real shadow repo, then a marker file that post-dates it.
    std::fs::write(workspace.path().join("kept.txt"), "v1").unwrap();
    let sg = liberado_coder_agent::ShadowGit::open_or_init(workspace.path(), &id).unwrap();
    let cp = sg.snapshot("pre-park").await.unwrap();
    std::fs::write(workspace.path().join("marker.txt"), "post-snapshot").unwrap();

    SessionRecordStore::push_event(
        store.as_ref(),
        SessionEvent::new(
            &id,
            SessionEventKind::Checkpoint {
                id: cp.id.clone(),
                label: cp.label.clone(),
                tree_hash: cp.tree_hash.clone(),
            },
        ),
    )
    .await;

    let response = post_rewind(&app, &id, None).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["checkpoint_id"], cp.id.as_str());
    assert_eq!(value["label"], cp.label.as_str());
    assert_eq!(value["restored"], true);

    assert!(
        !workspace.path().join("marker.txt").exists(),
        "restore rolls the work-tree back"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("kept.txt")).unwrap(),
        "v1"
    );
}

// ── the diff handler ────────────────────────────────────────────────────────────────

async fn get_diff(app: &Router, id: &str) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/goals/{id}/diff"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_text(response: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn diff_of_an_unknown_session_is_404() {
    let (app, _goals, _store) = rewind_app();
    let response = get_diff(&app, "g_ghost").await;
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    let text = body_text(response).await;
    assert!(text.contains("not found"), "{text}");
}

#[tokio::test]
async fn diff_of_a_session_without_a_workspace_is_404() {
    let (app, goals, _store) = rewind_app();
    let id = start_coding_session(&goals, serde_json::json!({})).await;
    let response = get_diff(&app, &id).await;
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    let text = body_text(response).await;
    assert!(text.contains("no workspace"), "{text}");
}

#[tokio::test]
async fn diff_with_a_workspace_that_vanished_is_404() {
    let (app, goals, _store) = rewind_app();
    let id = start_coding_session(
        &goals,
        serde_json::json!({ "workspace_root": "/nonexistent/diff-test-ws" }),
    )
    .await;
    let response = get_diff(&app, &id).await;
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    let text = body_text(response).await;
    assert!(text.contains("workspace not available"), "{text}");
}

#[tokio::test]
async fn diff_in_a_non_git_workspace_reports_git_failure_500() {
    let (app, goals, _store) = rewind_app();
    let workspace = tempfile::tempdir().unwrap();
    let id = start_coding_session(
        &goals,
        serde_json::json!({
            "workspace_root": workspace.path().to_string_lossy(),
        }),
    )
    .await;
    let response = get_diff(&app, &id).await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
    let text = body_text(response).await;
    assert!(text.contains("git diff failed"), "{text}");
}

#[tokio::test]
async fn diff_of_a_clean_workspace_says_no_changes() {
    let (app, goals, _store) = rewind_app();
    let workspace = tempfile::tempdir().unwrap();
    seed_git_repo(workspace.path());
    let id = start_coding_session(
        &goals,
        serde_json::json!({
            "workspace_root": workspace.path().to_string_lossy(),
        }),
    )
    .await;
    let response = get_diff(&app, &id).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let text = body_text(response).await;
    assert_eq!(text, "(no changes)");
}

/// A committed file plus an uncommitted edit: the diff endpoint returns exactly that edit.
#[tokio::test]
async fn diff_returns_the_uncommitted_change() {
    let (app, goals, _store) = rewind_app();
    let workspace = tempfile::tempdir().unwrap();
    seed_git_repo(workspace.path());
    std::fs::write(workspace.path().join("note.txt"), "line one\nedited\n").unwrap();
    let id = start_coding_session(
        &goals,
        serde_json::json!({
            "workspace_root": workspace.path().to_string_lossy(),
        }),
    )
    .await;
    let response = get_diff(&app, &id).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let text = body_text(response).await;
    assert!(text.contains("diff --git"), "{text}");
    assert!(text.contains("+edited"), "{text}");
}

/// `git init` a workspace with one committed file. Tests that shell out to git must set an
/// identity: CI runners have none (AGENTS.md).
fn seed_git_repo(dir: &std::path::Path) {
    use liberado_common::process::std_command;
    let git = |args: &[&str]| {
        std_command("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "liberado")
            .env("GIT_AUTHOR_EMAIL", "liberado@local")
            .env("GIT_COMMITTER_NAME", "liberado")
            .env("GIT_COMMITTER_EMAIL", "liberado@local")
            .output()
            .expect("git runs")
    };
    assert!(git(&["init", "--quiet"]).status.success());
    std::fs::write(dir.join("note.txt"), "line one\n").unwrap();
    assert!(git(&["add", "."]).status.success());
    assert!(git(&["commit", "--quiet", "-m", "seed"]).status.success());
}
