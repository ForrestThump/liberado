//! Unauthenticated outbox ack and owner push routes.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use liberado_outbox::{NewReminder, Outbox};
use tower::ServiceExt;

use super::outbox_ack_with_state;
use crate::state::AppState;

async fn app_with_outbox(outbox: Arc<Outbox>) -> Router {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(liberado_session_store::SessionStore::open(dir.path().join("sessions")).await);
    let mut state = AppState::for_test(sessions, None, dir.path().to_path_buf());
    state.outbox = Some(outbox);
    Router::new()
        .route(
            "/api/outbox/ack",
            axum::routing::post(outbox_ack_with_state),
        )
        .with_state(Arc::new(state))
}

#[tokio::test]
async fn outbox_ack_without_owner_cookie_is_open_and_constant_time() {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Arc::new(Outbox::open(&path).unwrap());
    let at = Utc::now();
    outbox
        .enqueue(
            &NewReminder {
                idempotency_key: "ack-http".into(),
                channel: "webpush".into(),
                target: "sub:1".into(),
                title: "t".into(),
                body: "b".into(),
                due_at: at,
                catch_up: false,
            },
            at,
        )
        .unwrap();
    let token = outbox.due_rows(at, 10).unwrap()[0].token.clone();
    let app = app_with_outbox(outbox.clone()).await;

    let wrong = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/outbox/ack")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"id":1,"token":"wrong"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::NO_CONTENT);
    assert!(!outbox.acked(1).unwrap());

    let ok = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/outbox/ack")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"id":1,"token":"{token}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::NO_CONTENT);
    assert!(outbox.acked(1).unwrap());
}
