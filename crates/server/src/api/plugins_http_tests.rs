use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use liberado_common::{Capability, CapabilitySet, Consequence, McpDescriptor};
use liberado_session_store::SessionStore;
use tower::ServiceExt;

use super::set_plugin_enabled;
use crate::api::catalog;
use crate::state::AppState;

async fn app(grant: bool) -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let mut state = AppState::for_test(sessions, None, dir.path().to_path_buf());
    state.catalog.register(McpDescriptor {
        name: "tasks".into(),
        description: "tasks".into(),
        consequence: Consequence::ReadOnly,
        ..McpDescriptor::default()
    });
    if grant {
        state.main_agent_capabilities =
            CapabilitySet::from_iter([Capability::ExecuteMcp("tasks".into())]);
    }
    let router = Router::new()
        .route("/api/plugins/{id}", axum::routing::put(set_plugin_enabled))
        .route("/api/catalog", axum::routing::get(catalog))
        .with_state(Arc::new(state));
    (router, dir)
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

#[tokio::test]
async fn unknown_server_is_404_and_a_grant_cannot_be_widened() {
    let (app, _dir) = app(false).await;
    let (status, _) = call(
        &app,
        "PUT",
        "/api/plugins/missing",
        Some(r#"{"enabled":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = call(
        &app,
        "PUT",
        "/api/plugins/tasks",
        Some(r#"{"enabled":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("cannot widen"), "{body}");
}

#[tokio::test]
async fn turning_chat_access_off_shows_on_the_catalog() {
    let (app, _dir) = app(true).await;
    let (status, body) = call(&app, "GET", "/api/catalog", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"visible_to_main_agent\":true"), "{body}");

    let (status, body) = call(
        &app,
        "PUT",
        "/api/plugins/tasks",
        Some(r#"{"enabled":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"visible_to_main_agent\":false"), "{body}");

    let (status, body) = call(&app, "GET", "/api/catalog", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"visible_to_main_agent\":false"), "{body}");
}
