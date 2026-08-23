//! Split from `goals.rs` for module-health boundaries.

//! S3/G4: coding goals refuse undeclared projects/paths at `POST /api/goals` (403).

use std::sync::Arc;
use std::time::Instant;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use liberado_common::{Capability, CapabilitySet, WriteClass, Zone};
use liberado_config::{Grant, ProjectConfig};
use liberado_session::{GoalSessionHub, GoalSessionStore, LifeOpsDemoRunner};
use tower::ServiceExt;

use crate::api::goals::{goals_start, list_projects};
use crate::state::AppState;

fn coding_capabilities() -> CapabilitySet {
    let mut caps = CapabilitySet::empty();
    caps.grant(Capability::AskHuman);
    caps.grant(Capability::Write(Zone::vault("tasks")));
    caps
}

fn config_with_project(project: ProjectConfig) -> liberado_bootstrap::Config {
    let mut config = liberado_bootstrap::Config::default();
    config.policy.grants.push(Grant {
        component: "coding".into(),
        capabilities: coding_capabilities().capabilities,
    });
    config.topology.projects.push(project);
    config
}

/// A stand-in for the coding pack that only records the goal it was handed.
///
/// Registering the real one would pull the whole `coder-agent` dependency tree into a server
/// test. What has to be observed here is narrow: the payload the daemon starts the session
/// with, after authorization has rewritten it.
struct RecordingCodingPack {
    seen: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

#[async_trait::async_trait]
impl liberado_session::DomainPackRunner for RecordingCodingPack {
    fn domain_id(&self) -> &str {
        liberado_session::CODING_DOMAIN
    }

    async fn run(
        &self,
        _session_id: &str,
        goal: &liberado_session::GoalSpec,
        _ctx: &liberado_session::PackContext<'_>,
        _events: tokio::sync::mpsc::Sender<liberado_session::SessionEvent>,
        _inputs: liberado_session::InputChannel,
        _cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<liberado_session::GoalResult, liberado_session::PackError> {
        self.seen.lock().unwrap().push(goal.payload.clone());
        Ok(liberado_session::GoalResult {
            terminal: liberado_session::TerminalKind::Succeeded,
            summary: "recorded".into(),
            artifacts: Vec::new(),
            diagnostics: serde_json::Value::Null,
        })
    }
}

/// `coding_goals_app`, plus a coding pack that records what it was started with.
fn coding_goals_app_recording(
    config: liberado_bootstrap::Config,
) -> (Router, Arc<std::sync::Mutex<Vec<serde_json::Value>>>) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = coding_goals_app_with(
        config,
        Some(Arc::new(RecordingCodingPack { seen: seen.clone() })),
    );
    (app, seen)
}

fn coding_goals_app(config: liberado_bootstrap::Config) -> Router {
    coding_goals_app_with(config, None)
}

/// The 403 paths never reach a pack, so the life demo alone is enough for them. Anything
/// asserting on what a *started* coding session received needs a pack answering to "coding".
fn coding_goals_app_with(
    config: liberado_bootstrap::Config,
    coding_pack: Option<Arc<RecordingCodingPack>>,
) -> Router {
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(LifeOpsDemoRunner));
    if let Some(pack) = coding_pack {
        hub.register_pack(pack);
    }
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
    });
    Router::new()
        .route("/api/goals", axum::routing::post(goals_start))
        .route("/api/projects", axum::routing::get(list_projects))
        .with_state(state)
}

async fn post_goal(app: &Router, body: serde_json::Value) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/goals")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn unknown_project_name_is_403() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let app = coding_goals_app(config_with_project(ProjectConfig {
        name: "liberado".into(),
        root,
        write_class: WriteClass::AgentWritable,
        enabled: true,
        preflight: Default::default(),
    }));
    let (status, body) = post_goal(
        &app,
        serde_json::json!({
            "description": "do a thing",
            "domain": "coding",
            "payload": { "project": "not-a-real-project", "interactive": true }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body.contains("unknown coding project") || body.contains("not-a-real-project"),
        "{body}"
    );
}

#[tokio::test]
async fn undeclared_workspace_path_is_403() {
    let declared = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(declared.path()).unwrap();
    let outside = std::fs::canonicalize(outside.path()).unwrap();
    let app = coding_goals_app(config_with_project(ProjectConfig {
        name: "liberado".into(),
        root,
        write_class: WriteClass::AgentWritable,
        enabled: true,
        preflight: Default::default(),
    }));
    let (status, body) = post_goal(
        &app,
        serde_json::json!({
            "description": "do a thing",
            "domain": "coding",
            "payload": {
                "workspace_root": outside.to_string_lossy(),
                "interactive": true
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body.contains("not under any declared") || body.contains("fail-closed"),
        "{body}"
    );
}

#[tokio::test]
async fn malformed_coding_payload_is_rejected_before_workspace_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let app = coding_goals_app(config_with_project(ProjectConfig {
        name: "liberado".into(),
        root,
        write_class: WriteClass::AgentWritable,
        enabled: true,
        preflight: Default::default(),
    }));
    let (status, body) = post_goal(
        &app,
        serde_json::json!({
            "description": "do a thing",
            "domain": "coding",
            "payload": { "workspace_root": 42 }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("invalid coding goal payload"), "{body}");
}

#[tokio::test]
async fn list_projects_returns_enabled_entries() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let app = coding_goals_app(config_with_project(ProjectConfig {
        name: "liberado".into(),
        root: root.clone(),
        write_class: WriteClass::AgentWritable,
        enabled: true,
        preflight: Default::default(),
    }));
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
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let projects = v["projects"].as_array().expect("projects array");
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["name"], "liberado");
    assert_eq!(
        projects[0]["root"].as_str().unwrap(),
        root.to_string_lossy().as_ref()
    );
}

#[tokio::test]
async fn an_authorized_project_name_reaches_the_pack_as_a_resolved_absolute_root() {
    // Naming a project is the entire point: `/goal in liberado` has to arrive at the pack as
    // that repo's path. Assert what the *pack* was started with, not the HTTP status — the
    // status is the same whether the root was injected or dropped, and dropping it does not
    // fail, it silently builds in a temp directory the human never asked for.
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (app, seen) = coding_goals_app_recording(config_with_project(ProjectConfig {
        name: "liberado".into(),
        root: root.clone(),
        write_class: WriteClass::AgentWritable,
        enabled: true,
        preflight: Default::default(),
    }));

    let (status, body) = post_goal(
        &app,
        serde_json::json!({
            "description": "do a thing",
            "domain": "coding",
            "payload": { "project": "liberado" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    // The pack runs on the hub's task; wait for it rather than racing it.
    let payload = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(p) = seen.lock().unwrap().first().cloned() {
                return p;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the coding pack should have been started");

    assert_eq!(
        payload["project"], "liberado",
        "the resolved project name must reach the pack: {payload}"
    );
    let injected = payload["workspace_root"]
        .as_str()
        .unwrap_or_else(|| panic!("no workspace_root reached the pack: {payload}"));
    // Server strips Windows `\\?\` so git/tools see a plain drive path.
    let expected = super::strip_windows_extended_path(&root);
    assert_eq!(
        std::path::Path::new(injected),
        std::path::Path::new(&expected),
        "the pack must receive the project's resolved absolute root"
    );
}

#[tokio::test]
async fn a_client_supplied_workspace_root_is_replaced_by_the_resolved_one() {
    // The payload field is caller-controlled. Authorization has to *overwrite* it, not merely
    // approve it — otherwise a non-canonical spelling of an allowed path is what the pack acts
    // on, and the string that was checked is not the string that is used.
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let sub = root.join("crates");
    std::fs::create_dir_all(&sub).unwrap();
    let (app, seen) = coding_goals_app_recording(config_with_project(ProjectConfig {
        name: "liberado".into(),
        root: root.clone(),
        write_class: WriteClass::AgentWritable,
        enabled: true,
        preflight: Default::default(),
    }));

    // Built as a *string*, not by `PathBuf::join`: pushing `..` onto a verbatim `\?\` path
    // collapses it at construction on Windows, so a joined path would arrive already canonical
    // and the test could not tell the two apart.
    let sep = std::path::MAIN_SEPARATOR;
    let scenic = format!("{}{sep}crates{sep}..{sep}crates", root.display());
    let (status, body) = post_goal(
        &app,
        serde_json::json!({
            "description": "do a thing",
            "domain": "coding",
            "payload": { "workspace_root": scenic }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let payload = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(p) = seen.lock().unwrap().first().cloned() {
                return p;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the coding pack should have been started");

    let expected = super::strip_windows_extended_path(&sub);
    assert_eq!(
        std::path::Path::new(payload["workspace_root"].as_str().unwrap()),
        std::path::Path::new(&expected),
        "the pack must get the canonical path, not the caller's spelling: {payload}"
    );
    assert_eq!(payload["project"], "liberado", "{payload}");
}

#[tokio::test]
async fn life_domain_ignores_project_payload() {
    let mut config = liberado_bootstrap::Config::default();
    config.policy.grants.push(Grant {
        component: "life".into(),
        capabilities: coding_capabilities().capabilities,
    });
    let app = coding_goals_app(config);
    let (status, body) = post_goal(
        &app,
        serde_json::json!({
            "description": "capture a note",
            "domain": "life",
            "payload": { "project": "does-not-exist", "interactive": true }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}
