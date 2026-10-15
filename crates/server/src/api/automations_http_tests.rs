//! HTTP tests for read-only schedule and hook list routes.

use std::sync::Arc;
use std::time::Instant;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use liberado_common::CapabilitySet;
use liberado_config::{CronSchedule, HookConfig};
use liberado_session::{GoalSessionHub, GoalSessionStore, LifeOpsDemoRunner};
use tower::ServiceExt;

use crate::api::automations::{list_hooks, list_schedules};
use crate::state::AppState;

fn app_with_topology(schedules: Vec<CronSchedule>, hooks: Vec<HookConfig>) -> Router {
    let mut config = liberado_bootstrap::Config::default();
    config.topology.schedules = schedules;
    config.topology.hooks = hooks;

    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(LifeOpsDemoRunner));
    let goals = Arc::new(hub);
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
        main_agent_capabilities: CapabilitySet::empty(),
        dispatcher_capabilities: CapabilitySet::empty(),
        config: Arc::new(config),
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
    Router::new()
        .route("/api/schedules", axum::routing::get(list_schedules))
        .route("/api/hooks", axum::routing::get(list_hooks))
        .with_state(state)
}

#[tokio::test]
async fn list_schedules_reflects_topology_rows() {
    let app = app_with_topology(
        vec![CronSchedule {
            name: "morning-brief".into(),
            enabled: true,
            cron_expr: "0 0 9 * * * *".into(),
            goal: "Summarize inbox".into(),
            pool: None,
            profile: Some("maintenance".into()),
            deliver: None,
            max_turns: None,
            job: None,
            habit_text: None,
            task_limit: None,
            capture_path: None,
            git_remote: None,
            git_branch: None,
            git_user_name: None,
            git_user_email: None,
            run_on_start: None,
            direct: None,
        }],
        vec![],
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/schedules")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let rows = v["schedules"].as_array().expect("schedules");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "morning-brief");
    assert_eq!(rows[0]["cron_expr"], "0 0 9 * * * *");
    assert_eq!(rows[0]["profile"], "maintenance");
    assert!(rows[0]["next_fire"].is_null());
}

#[tokio::test]
async fn list_hooks_never_returns_secret_ref() {
    let app = app_with_topology(
        vec![],
        vec![HookConfig {
            name: "nightly-backup".into(),
            enabled: true,
            secret_ref: "NIGHTLY_HOOK_SECRET".into(),
            goal: "run backup".into(),
            pool: None,
            profile: None,
        }],
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/hooks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!text.contains("NIGHTLY_HOOK_SECRET"));
    assert!(!text.contains("secret_ref"));
    let v: serde_json::Value = serde_json::from_slice(bytes.as_ref()).unwrap();
    assert_eq!(v["hooks"][0]["name"], "nightly-backup");
}
