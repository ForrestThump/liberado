//! HTTP and scan tests for `GET /api/library`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use liberado_common::{Capability, CapabilitySet, Zone};
use liberado_config::{Grant, SessionProfile};
use liberado_session::{GoalSessionHub, GoalSessionStore};
use tower::ServiceExt;

use super::{LibraryItem, artifact_zone, clamped_limit, library_zones, list_bounded, list_library};
use crate::state::AppState;

fn read_grant(component: &str, zones: &[&str]) -> Grant {
    let mut caps = CapabilitySet::empty();
    for zone in zones {
        caps.grant(Capability::Read(Zone::vault(*zone)));
    }
    Grant {
        component: component.into(),
        capabilities: caps.capabilities,
    }
}

fn config_with(grants: Vec<Grant>, profiles: Vec<SessionProfile>) -> liberado_bootstrap::Config {
    let mut config = liberado_bootstrap::Config::default();
    config.policy.grants = grants;
    config.topology.session_profiles = profiles;
    config
}

fn borrowing(component: &str) -> SessionProfile {
    let mut profile = SessionProfile::empty("chat-default");
    profile.component = Some(component.into());
    profile
}

fn reading(zones: &[&str]) -> SessionProfile {
    let mut profile = SessionProfile::empty("chat-default");
    profile.read = zones.iter().map(|zone| (*zone).to_string()).collect();
    profile
}

fn app(vault: &Path, config: liberado_bootstrap::Config) -> Router {
    let goals = Arc::new(GoalSessionHub::new(GoalSessionStore::new()));
    let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(AppState {
        start_time: std::time::Instant::now(),
        reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        dispatcher_attached: false,
        orchestrator_attached: false,
        watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vault_path: vault.to_string_lossy().into_owned(),
        goals,
        chat: None,
        chat_tools: 0,
        chat_tool_names: Vec::new(),
        catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
        data_dir: PathBuf::from("/tmp/liberado"),
        sessions_root: PathBuf::from("/tmp/liberado/sessions"),
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
        surfaces: crate::surfaces::LiveSurfaces::inert(),
    });
    Router::new()
        .route("/api/library", get(list_library))
        .with_state(state)
}

fn write_file(path: &Path, body: &str, age: Duration) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - age).unwrap();
}

async fn get_library(app: &Router, query: &str) -> (StatusCode, Vec<LibraryItem>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/library{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: super::LibraryResponse = serde_json::from_slice(&bytes).unwrap();
    (status, parsed.items)
}

#[test]
fn limit_defaults_to_50_and_stays_in_range() {
    assert_eq!(clamped_limit(None), 50);
    assert_eq!(clamped_limit(Some(0)), 1);
    assert_eq!(clamped_limit(Some(50)), 50);
    assert_eq!(clamped_limit(Some(10_000)), 200);
}

#[test]
fn artifact_names_match_the_v1_folder_set() {
    assert!(artifact_zone("attachments"));
    assert!(artifact_zone("Media"));
    assert!(artifact_zone("images"));
    assert!(artifact_zone("files"));
    assert!(artifact_zone("exports"));
    assert!(artifact_zone("artifacts"));
    assert!(!artifact_zone("tasks"));
    assert!(!artifact_zone("decisions"));
    assert!(!artifact_zone("attachments/../etc"));
}

#[test]
fn chat_default_read_zones_win_and_tasks_are_not_library_folders() {
    let config = config_with(
        vec![read_grant(
            "main-agent",
            &["tasks", "attachments", "attachments"],
        )],
        vec![borrowing("main-agent")],
    );
    assert_eq!(library_zones(&config), vec!["attachments".to_string()]);
}

#[test]
fn a_profile_that_declares_its_own_reads_does_not_inherit_the_component() {
    let config = config_with(
        vec![read_grant("main-agent", &["tasks", "media"])],
        vec![reading(&["images"])],
    );
    assert_eq!(library_zones(&config), vec!["images".to_string()]);
}

#[test]
fn a_disabled_chat_default_falls_back_to_the_main_agent_grant() {
    let mut profile = borrowing("main-agent");
    profile.enabled = false;
    let config = config_with(
        vec![read_grant("main-agent", &["media", "tasks"])],
        vec![profile],
    );
    assert_eq!(library_zones(&config), vec!["media".to_string()]);
}

#[test]
fn no_chat_default_uses_the_main_agent_grant() {
    let config = config_with(vec![read_grant("main-agent", &["exports"])], Vec::new());
    assert_eq!(library_zones(&config), vec!["exports".to_string()]);
}

#[test]
fn write_only_and_named_zones_are_not_library_folders() {
    let mut write_only = CapabilitySet::empty();
    write_only.grant(Capability::Write(Zone::vault("attachments")));
    let mut named = CapabilitySet::empty();
    named.grant(Capability::Read(Zone::named("media")));
    let config = config_with(
        vec![Grant {
            component: "main-agent".into(),
            capabilities: write_only
                .capabilities
                .into_iter()
                .chain(named.capabilities)
                .collect(),
        }],
        vec![borrowing("main-agent")],
    );
    assert!(library_zones(&config).is_empty());
}

#[test]
fn the_scan_keeps_visible_artifact_files_and_skips_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_file(
        &root.join("attachments/old.txt"),
        "old",
        Duration::from_secs(3_600),
    );
    write_file(
        &root.join("attachments/nested/Photo.PNG"),
        "pic",
        Duration::from_secs(60),
    );
    write_file(
        &root.join("attachments/.secret"),
        "no",
        Duration::from_secs(1),
    );
    write_file(
        &root.join("attachments/.hidden/note.txt"),
        "no",
        Duration::from_secs(1),
    );
    write_file(&root.join("tasks/todo.md"), "task", Duration::from_secs(1));
    write_file(&root.join("README"), "root", Duration::from_secs(1));

    let mut deep = root.join("attachments");
    for level in 0..9 {
        deep = deep.join(format!("d{level}"));
    }
    write_file(&deep.join("too-deep.txt"), "deep", Duration::from_secs(1));

    let zones = vec!["attachments".to_string()];
    let items = list_bounded(root, &zones, 50, 2_000);
    let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["attachments/nested/Photo.PNG", "attachments/old.txt"]
    );
    assert_eq!(items[0].title, "Photo.PNG");
    assert_eq!(items[0].kind, "png");
    assert_eq!(items[0].path_or_url, items[0].id);
    assert_eq!(items[0].source, "vault");
    assert!(items[0].created_at.ends_with('Z'));
    assert!(items[0].created_at.contains('T'));
    assert_eq!(items[1].kind, "txt");

    let newest = list_bounded(root, &zones, 1, 2_000);
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0].id, "attachments/nested/Photo.PNG");

    let capped = list_bounded(root, &zones, 50, 1);
    assert_eq!(capped.len(), 1);
}

#[test]
fn a_file_with_no_extension_is_kind_file() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        &dir.path().join("files/README"),
        "x",
        Duration::from_secs(1),
    );
    let items = list_bounded(dir.path(), &["files".to_string()], 10, 10);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, "file");
    assert_eq!(items[0].title, "README");
}

#[test]
fn a_missing_vault_is_an_empty_list() {
    let missing = tempfile::tempdir().unwrap();
    let path = missing.path().join("does-not-exist");
    let items = list_bounded(&path, &["media".to_string()], 50, 10);
    assert!(items.is_empty());
}

#[tokio::test]
async fn get_library_returns_vault_items_and_honors_limit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_file(
        &root.join("attachments/newer.md"),
        "new",
        Duration::from_secs(30),
    );
    write_file(
        &root.join("attachments/older.md"),
        "old",
        Duration::from_secs(300),
    );
    write_file(&root.join("tasks/todo.md"), "task", Duration::from_secs(1));
    let config = config_with(
        vec![read_grant("main-agent", &["attachments", "tasks"])],
        vec![borrowing("main-agent")],
    );
    let app = app(root, config);

    let (status, items) = get_library(&app, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, "attachments/newer.md");
    assert_eq!(items[0].source, "vault");
    assert_eq!(items[1].id, "attachments/older.md");
    assert!(items.iter().all(|item| item.source == "vault"));
    assert!(items.iter().all(|item| !item.id.contains("tasks")));

    let (status, one) = get_library(&app, "?limit=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].id, "attachments/newer.md");
}

#[test]
fn the_server_registers_get_library() {
    let src = include_str!("../lib.rs");
    assert!(src.contains("/api/library"));
    assert!(src.contains("api::list_library"));
    assert!(!src.contains("post(api::list_library)"));
}
