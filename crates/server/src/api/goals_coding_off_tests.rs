//! Default-build checks for coding entry points. These compile only when the `coding` feature
//! is off, which is what `cargo test` does.

use super::*;
use axum::Router;
use axum::body::Body;
use axum::http::Request;
use liberado_session::{
    DomainHint, DomainPackRunner, GoalResult, GoalSessionHub, GoalSessionStore, GoalSpec,
    InputChannel, LifeOpsDemoRunner, PackContext, PackError, SessionEvent, TerminalKind,
};
use std::time::Instant;
use tower::ServiceExt;

struct FinishImmediately;

#[async_trait::async_trait]
impl DomainPackRunner for FinishImmediately {
    fn domain_id(&self) -> &str {
        liberado_session::CODING_DOMAIN
    }

    async fn run(
        &self,
        _session_id: &str,
        _goal: &GoalSpec,
        _ctx: &PackContext<'_>,
        _events: tokio::sync::mpsc::Sender<SessionEvent>,
        _inputs: InputChannel,
        _cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        Ok(GoalResult {
            terminal: TerminalKind::Succeeded,
            summary: "done".into(),
            artifacts: Vec::new(),
            diagnostics: serde_json::Value::Null,
        })
    }
}

fn app() -> (Router, Arc<GoalSessionHub>) {
    app_with(liberado_bootstrap::Config::default())
}

fn app_with(config: liberado_bootstrap::Config) -> (Router, Arc<GoalSessionHub>) {
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(LifeOpsDemoRunner));
    hub.register_pack(Arc::new(FinishImmediately));
    let goals = Arc::new(hub);
    let config = Arc::new(config);
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
        catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
        data_dir: std::path::PathBuf::from("/tmp/liberado"),
        sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
        main_agent_capabilities: liberado_common::CapabilitySet::empty(),
        dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
        config: config.clone(),
        surfaces: crate::surfaces::LiveSurfaces::memory(config),
        sessions: Arc::new(Default::default()),
        model_name: None,
        provider: None,
        hooks: std::collections::HashMap::new(),
        hook_tx,
        hook_idempotency: crate::hooks::IdempotencyCache::default(),
        live_mcp: liberado_bootstrap::LiveMcpController::empty(),
        drain: crate::shutdown::DrainGate::default(),
        approval_hub: None,
    });
    let app = Router::new()
        .route("/api/goals", axum::routing::post(goals_start))
        .route("/api/goals/domains", axum::routing::get(goals_domains))
        .route("/api/goals/{id}/rewind", axum::routing::post(goals_rewind))
        .route("/api/projects", axum::routing::get(list_projects))
        .with_state(state);
    (app, goals)
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

#[tokio::test]
async fn post_coding_goal_names_the_missing_feature() {
    let (app, _) = app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/goals")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"description":"ship it","domain":"coding","success_criteria":[]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_text(response).await;
    assert!(
        body.contains("`coding`"),
        "feature sentence must name `coding`, got {body}"
    );
}

#[tokio::test]
async fn rewind_of_a_coding_session_names_the_missing_feature() {
    let (app, goals) = app();
    let id = goals
        .start(GoalSpec {
            id: None,
            description: "rewind me".into(),
            success_criteria: vec![],
            domain: DomainHint::Coding,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            direct: false,
            payload: serde_json::json!({}),
        })
        .await
        .expect("stub pack accepts the domain");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/goals/{id}/rewind"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_text(response).await;
    assert!(body.contains("`coding`"), "{body}");
}

#[tokio::test]
async fn domains_list_omits_coding_when_the_pack_is_not_registered() {
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(LifeOpsDemoRunner));
    let goals = Arc::new(hub);
    let config = Arc::new(liberado_bootstrap::Config::default());
    let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(AppState {
        start_time: Instant::now(),
        reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        dispatcher_attached: false,
        orchestrator_attached: false,
        watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vault_path: "/tmp/vault".to_string(),
        goals,
        chat: None,
        chat_tools: 0,
        chat_tool_names: Vec::new(),
        catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
        data_dir: std::path::PathBuf::from("/tmp/liberado"),
        sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
        main_agent_capabilities: liberado_common::CapabilitySet::empty(),
        dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
        config: config.clone(),
        surfaces: crate::surfaces::LiveSurfaces::memory(config),
        sessions: Arc::new(Default::default()),
        model_name: None,
        provider: None,
        hooks: std::collections::HashMap::new(),
        hook_tx,
        hook_idempotency: crate::hooks::IdempotencyCache::default(),
        live_mcp: liberado_bootstrap::LiveMcpController::empty(),
        drain: crate::shutdown::DrainGate::default(),
        approval_hub: None,
    });
    let app = Router::new()
        .route("/api/goals/domains", axum::routing::get(goals_domains))
        .with_state(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/goals/domains")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains("life"), "{body}");
    assert!(!body.contains("coding"), "{body}");
}

#[tokio::test]
async fn list_projects_returns_enabled_entries_without_the_coding_feature() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical project root");
    let mut config = liberado_bootstrap::Config::default();
    config
        .topology
        .projects
        .push(liberado_config::ProjectConfig {
            name: "liberado".into(),
            root: root.clone(),
            write_class: liberado_common::WriteClass::AgentWritable,
            enabled: true,
            preflight: Default::default(),
        });
    let (app, _) = app_with(config);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/projects")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    let value: serde_json::Value = serde_json::from_str(&body).expect("projects json");
    let projects = value["projects"].as_array().expect("projects array");
    assert_eq!(projects.len(), 1, "{body}");
    assert_eq!(projects[0]["name"], "liberado");
    assert_eq!(
        projects[0]["root"].as_str().unwrap(),
        root.to_string_lossy()
    );
}
