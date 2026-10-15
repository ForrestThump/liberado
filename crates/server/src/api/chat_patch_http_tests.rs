//! HTTP contract for project and pending-model on a conversation.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use liberado_common::WriteClass;
use liberado_config::ProjectConfig;
use liberado_executor::{Budget, Executor};
use liberado_main_agent::ChatSessions;
use liberado_provider::{CompletionResponse, MockProvider};
use liberado_session_store::SessionStore;
use liberado_test_support::NoopRuntime;
use tower::ServiceExt;

use crate::state::AppState;

struct Harness {
    app: Router,
    _dir: tempfile::TempDir,
}

async fn harness(projects: Vec<ProjectConfig>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let chat = Arc::new(ChatSessions::new(
        sessions.clone(),
        Executor::new(
            Arc::new(MockProvider::with_script(
                "mock",
                [CompletionResponse::text("ok")],
            )),
            Budget::default(),
        ),
        Arc::new(NoopRuntime),
    ));
    let mut config = liberado_bootstrap::Config::default();
    config.topology.projects = projects;
    let mut state = AppState::for_test(sessions, Some(chat), dir.path().to_path_buf());
    state.config = Arc::new(config);
    let app = Router::new()
        .route(
            "/api/conversations",
            axum::routing::get(super::super::list_conversations)
                .post(crate::api::create_conversation),
        )
        .route(
            "/api/conversations/{id}",
            axum::routing::get(super::super::get_conversation)
                .patch(super::super::patch_conversation_title),
        )
        .route("/api/chat", axum::routing::post(super::super::chat))
        .with_state(Arc::new(state));
    Harness { app, _dir: dir }
}

fn project(name: &str) -> ProjectConfig {
    ProjectConfig {
        name: name.into(),
        root: std::env::temp_dir(),
        write_class: WriteClass::AgentWritable,
        enabled: true,
        preflight: Default::default(),
    }
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    let request = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        builder.body(Body::from(body.to_string()))
    } else {
        builder.body(Body::empty())
    }
    .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap()
}

async fn create(app: &Router) -> String {
    let (status, body) = call(app, "POST", "/api/conversations", Some("{}")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    json(&body)["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn title_project_and_pending_model_round_trip_on_get() {
    let h = harness(vec![project("liberado")]).await;
    let id = create(&h.app).await;

    let (status, _) = call(
        &h.app,
        "PATCH",
        &format!("/api/conversations/{id}"),
        Some(r#"{"title":"Notes"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(&h.app, "GET", "/api/conversations", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = json(&body)
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap()
        .clone();
    assert_eq!(row["title"], "Notes");
    assert!(row.get("project").is_none());
    assert!(row.get("pending_model").is_none());

    let (status, body) = call(
        &h.app,
        "PATCH",
        &format!("/api/conversations/{id}"),
        Some(r#"{"project":"nope"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("unknown project"));

    let (status, _) = call(
        &h.app,
        "PATCH",
        &format!("/api/conversations/{id}"),
        Some(r#"{"project":"liberado","pending_model":"vendor/exact-model"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(&h.app, "GET", &format!("/api/conversations/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = json(&body);
    assert_eq!(detail["project"], "liberado");
    assert_eq!(detail["pending_model"], "vendor/exact-model");

    let (status, body) = call(&h.app, "GET", "/api/conversations", None).await;
    let row = json(&body)
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap()
        .clone();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(row["project"], "liberado");
    assert_eq!(row["pending_model"], "vendor/exact-model");
    assert_eq!(row["title"], "Notes");

    let (status, _) = call(
        &h.app,
        "PATCH",
        &format!("/api/conversations/{id}"),
        Some(r#"{"project":null}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(&h.app, "GET", &format!("/api/conversations/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = json(&body);
    assert!(detail.get("project").is_none());
    assert_eq!(detail["pending_model"], "vendor/exact-model");
}

#[tokio::test]
async fn the_next_turn_stamps_the_pending_model_and_clears_it() {
    let h = harness(vec![project("liberado")]).await;
    let id = create(&h.app).await;
    let (status, _) = call(
        &h.app,
        "PATCH",
        &format!("/api/conversations/{id}"),
        Some(r#"{"pending_model":"vendor/exact-model"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(
        &h.app,
        "POST",
        "/api/chat",
        Some(&format!(r#"{{"message":"hello","session":"{id}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(&h.app, "GET", &format!("/api/conversations/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = json(&body);
    assert!(detail.get("pending_model").is_none());
    let models: Vec<&str> = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["model"].as_str())
        .collect();
    assert!(
        models.contains(&"vendor/exact-model"),
        "turn substituted the pick: {models:?}"
    );
    assert!(
        models.iter().all(|model| *model == "vendor/exact-model"),
        "turn stamped a different model: {models:?}"
    );
}

#[tokio::test]
async fn a_request_model_is_not_replaced_by_a_stored_pick() {
    let h = harness(vec![]).await;
    let id = create(&h.app).await;
    let (status, _) = call(
        &h.app,
        "PATCH",
        &format!("/api/conversations/{id}"),
        Some(r#"{"pending_model":"stored-model"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(
        &h.app,
        "POST",
        "/api/chat",
        Some(&format!(
            r#"{{"message":"hello","session":"{id}","model":"request-model"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, body) = call(&h.app, "GET", &format!("/api/conversations/{id}"), None).await;
    let detail = json(&body);
    assert!(detail.get("pending_model").is_none());
    let user = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap();
    assert_eq!(user["model"], "request-model");
}
