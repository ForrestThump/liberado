//! HTTP contract for creating a conversation, posting a turn, and reading it back.
//!
//! `POST /api/conversations` opens the log. `POST /api/chat` appends a turn.
//! `GET /api/conversations/{id}` returns that log in send order.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use liberado_conversation_store::Ulid;
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

async fn harness(script: Vec<CompletionResponse>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let chat = Arc::new(ChatSessions::new(
        sessions.clone(),
        Executor::new(
            Arc::new(MockProvider::with_script("mock", script)),
            Budget::default(),
        ),
        Arc::new(NoopRuntime),
    ));
    let state = Arc::new(AppState::for_test(
        sessions,
        Some(chat),
        dir.path().to_path_buf(),
    ));
    let app = Router::new()
        .route("/api/chat", axum::routing::post(super::chat))
        .route(
            "/api/conversations",
            axum::routing::post(crate::api::create_conversation),
        )
        .route(
            "/api/conversations/{id}",
            axum::routing::get(super::get_conversation),
        )
        .with_state(state);
    Harness { app, _dir: dir }
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
    serde_json::from_str(body).unwrap_or_else(|err| panic!("body is not json ({err}): {body}"))
}

#[tokio::test]
async fn create_conversation_is_readable() {
    let h = harness(vec![]).await;
    let (status, body) = call(&h.app, "POST", "/api/conversations", Some("{}")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created = json(&body);
    let id = created["id"].as_str().unwrap();
    assert!(id.len() > 10, "{body}");
    assert_eq!(created["surface_mode"], "chat");

    let (status, body) = call(&h.app, "GET", &format!("/api/conversations/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let history = json(&body);
    let messages = history["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system", "{body}");
    assert_eq!(history["turn_running"], false);
}

#[tokio::test]
async fn posted_turns_come_back_in_send_order() {
    let h = harness(vec![
        CompletionResponse::text("alpha"),
        CompletionResponse::text("beta"),
    ])
    .await;
    let (status, body) = call(&h.app, "POST", "/api/conversations", Some("{}")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = json(&body)["id"].as_str().unwrap().to_string();

    let (status, body) = call(
        &h.app,
        "POST",
        "/api/chat",
        Some(&format!(r#"{{"message":"one","session":"{id}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let first = json(&body);
    assert_eq!(first["reply"], "alpha");
    assert_eq!(first["session"], id);

    let (status, body) = call(
        &h.app,
        "POST",
        "/api/chat",
        Some(&format!(r#"{{"message":"two","session":"{id}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json(&body)["reply"], "beta");

    let (status, body) = call(&h.app, "GET", &format!("/api/conversations/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let messages = json(&body)["messages"].as_array().unwrap().clone();
    let spoken: Vec<(String, String)> = messages
        .iter()
        .filter(|message| message["role"] != "system")
        .map(|message| {
            (
                message["role"].as_str().unwrap().to_string(),
                message["content"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        spoken,
        vec![
            ("user".to_string(), "one".to_string()),
            ("assistant".to_string(), "alpha".to_string()),
            ("user".to_string(), "two".to_string()),
            ("assistant".to_string(), "beta".to_string()),
        ]
    );
    assert_eq!(messages[0]["role"], "system");
}

#[tokio::test]
async fn a_missing_conversation_is_not_found() {
    let h = harness(vec![]).await;
    let id = Ulid::new();
    let (status, body) = call(&h.app, "GET", &format!("/api/conversations/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(json(&body)["error"], "conversation not found");
}

#[tokio::test]
async fn a_conversation_id_that_is_not_a_ulid_is_rejected() {
    let h = harness(vec![]).await;
    let (status, _) = call(&h.app, "GET", "/api/conversations/not-a-ulid", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_chat_body_that_is_not_json_is_rejected() {
    let h = harness(vec![]).await;
    let (status, _) = call(&h.app, "POST", "/api/chat", Some("{not json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_chat_body_without_a_message_is_rejected() {
    let h = harness(vec![]).await;
    let (status, _) = call(&h.app, "POST", "/api/chat", Some("{}")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_chat_session_that_is_not_a_ulid_is_rejected() {
    let h = harness(vec![]).await;
    let (status, _) = call(
        &h.app,
        "POST",
        "/api/chat",
        Some(r#"{"message":"hi","session":"nope"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn posting_to_an_unknown_session_is_an_error() {
    let h = harness(vec![CompletionResponse::text("should not run")]).await;
    let id = Ulid::new();
    let (status, body) = call(
        &h.app,
        "POST",
        "/api/chat",
        Some(&format!(r#"{{"message":"hi","session":"{id}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    let error = json(&body)["error"].as_str().unwrap().to_string();
    assert!(
        error.contains("not found"),
        "missing session must surface the store error, got {error}"
    );
}

#[tokio::test]
async fn a_create_body_that_is_not_json_is_rejected() {
    let h = harness(vec![]).await;
    let (status, _) = call(&h.app, "POST", "/api/conversations", Some("{not json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_create_body_with_a_bad_profile_type_is_rejected() {
    let h = harness(vec![]).await;
    let (status, _) = call(
        &h.app,
        "POST",
        "/api/conversations",
        Some(r#"{"profile":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
