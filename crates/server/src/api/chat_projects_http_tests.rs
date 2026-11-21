//! HTTP contract for user chat projects and conversation attachment.

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

struct Held {
    app: Router,
    _dir: tempfile::TempDir,
}

async fn app(projects: Vec<ProjectConfig>) -> Held {
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
            "/api/projects",
            axum::routing::get(crate::api::list_projects),
        )
        .route(
            "/api/chat-projects",
            axum::routing::get(crate::api::list_chat_projects)
                .post(crate::api::create_chat_project),
        )
        .route(
            "/api/chat-projects/{id}/chats",
            axum::routing::get(crate::api::list_project_chats),
        )
        .route(
            "/api/chat-projects/{id}",
            axum::routing::patch(crate::api::patch_chat_project)
                .delete(crate::api::delete_chat_project),
        )
        .route(
            "/api/conversations",
            axum::routing::post(crate::api::create_conversation),
        )
        .route(
            "/api/conversations/{id}",
            axum::routing::get(crate::api::get_conversation)
                .patch(crate::api::patch_conversation_title),
        )
        .with_state(Arc::new(state));
    Held { app, _dir: dir }
}

fn topology(name: &str) -> ProjectConfig {
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

#[tokio::test]
async fn a_user_project_is_not_a_coding_root_and_delete_only_detaches() {
    let held = app(vec![topology("liberado")]).await;
    let app = held.app.clone();

    let (status, body) = call(
        &app,
        "POST",
        "/api/chat-projects",
        Some(r#"{"name":"Trip notes","instructions":"Pack light"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created = json(&body);
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["source"], "user");
    assert!(created.get("root").is_none());
    assert!(created.get("write_class").is_none());

    let config = liberado_bootstrap::Config::default();
    assert!(config.authorize_coding_workspace(Some(&id), None).is_err());

    let (status, body) = call(&app, "GET", "/api/projects", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains(&id));
    assert!(!body.contains("Trip notes"));

    let (status, body) = call(&app, "GET", "/api/chat-projects", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = json(&body);
    let names: Vec<&str> = listed["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["liberado", "Trip notes"]);

    let (status, body) = call(
        &app,
        "POST",
        "/api/conversations",
        Some(&format!(r#"{{"title":"Packing list","project":"{id}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let chat = json(&body)["id"].as_str().unwrap().to_string();
    let (status, body) = call(&app, "GET", &format!("/api/conversations/{chat}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json(&body)["project"], id);

    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/conversations/{chat}"),
        Some(r#"{"project":"not-a-project"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = call(&app, "GET", &format!("/api/chat-projects/{id}/chats"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json(&body)["chats"][0]["title"], "Packing list");

    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/chat-projects/{id}"),
        Some(r#"{"name":"Trip plan"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json(&body)["name"], "Trip plan");

    let (status, _) = call(&app, "DELETE", "/api/chat-projects/liberado", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(&app, "DELETE", &format!("/api/chat-projects/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = call(&app, "GET", &format!("/api/conversations/{chat}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(json(&body).get("project").is_none() || json(&body)["project"].is_null());
    let (status, _) = call(&app, "DELETE", &format!("/api/chat-projects/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
