//! `GET /api/completions` is owner-only and read-only.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{TimeZone, Utc};
use liberado_outbox::{Outbox, liberado_tap_completion};
use tower::ServiceExt;

use super::list_completions;
use crate::auth::{ApiAuth, ApiGuard};
use crate::state::AppState;

async fn app(outbox: Arc<Outbox>) -> Router {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(liberado_session_store::SessionStore::open(dir.path().join("sessions")).await);
    let mut state = AppState::for_test(sessions, None, dir.path().to_path_buf());
    state.outbox = Some(outbox);
    let state = Arc::new(state);
    let guard = ApiGuard::with_auth(ApiAuth::from_values(Some("owner-token-test-000000"), &[]));
    Router::new()
        .route("/api/completions", axum::routing::get(list_completions))
        .route_layer(axum::middleware::from_fn(crate::auth::require_owner))
        .route_layer(axum::middleware::from_fn_with_state(
            guard,
            crate::auth::require_credential,
        ))
        .with_state(state)
}

fn owner_get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", "Bearer owner-token-test-000000")
        .body(Body::empty())
        .unwrap()
}

fn anon_get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn completions_require_a_session_and_return_stored_rows() {
    let dir = tempfile::tempdir().unwrap();
    let outbox = Arc::new(Outbox::open(&dir.path().join("liberado.db")).unwrap());
    let when = Utc.with_ymd_and_hms(2026, 11, 10, 15, 0, 0).unwrap();
    let row = liberado_tap_completion("Home.md", "Call dentist", 4, when, "UTC");
    assert!(outbox.insert_completion(&row).unwrap());
    let app = app(outbox).await;

    let anon = app
        .clone()
        .oneshot(anon_get("/api/completions"))
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(owner_get("/api/completions"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["source"], "liberado");
    assert_eq!(rows[0]["precision"], "tap");
    assert_eq!(rows[0]["outbox_id"], 4);
    assert_eq!(rows[0]["task_text"], "Call dentist");
    assert_eq!(rows[0]["path"], "Home.md");
    assert!(rows[0]["voided_at"].is_null());

    let later = app
        .clone()
        .oneshot(owner_get("/api/completions?since=2026-11-11T00:00:00Z"))
        .await
        .unwrap();
    assert_eq!(later.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(later.into_body(), usize::MAX)
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    assert!(rows.is_empty());

    let bad = app
        .oneshot(owner_get("/api/completions?since=yesterday"))
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
}
