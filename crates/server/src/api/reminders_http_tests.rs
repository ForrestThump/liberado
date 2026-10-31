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

use super::{
    delete_subscription, list_subscriptions, outbox_ack_with_state, push_status, push_test,
    report_client_error, upsert_subscription, vapid_public_key,
};
use crate::auth::{ApiAuth, ApiGuard};
use crate::state::AppState;

const TEST_P256DH: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const TEST_AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

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

async fn owner_push_app(outbox: Arc<Outbox>) -> Router {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(liberado_session_store::SessionStore::open(dir.path().join("sessions")).await);
    let mut state = AppState::for_test(sessions, None, dir.path().to_path_buf());
    state.outbox = Some(outbox);
    let state = Arc::new(state);
    let guard = Arc::new(ApiGuard::new(
        ApiAuth::from_values(Some("owner-token-test-000000"), &[]),
        Vec::new(),
    ));
    Router::new()
        .route("/api/push/status", axum::routing::get(push_status))
        .route(
            "/api/push/vapid-public-key",
            axum::routing::get(vapid_public_key),
        )
        .route(
            "/api/push/subscriptions",
            axum::routing::get(list_subscriptions)
                .post(upsert_subscription)
                .delete(delete_subscription),
        )
        .route("/api/push/test", axum::routing::post(push_test))
        .route(
            "/api/client-errors",
            axum::routing::post(report_client_error),
        )
        .route_layer(axum::middleware::from_fn(crate::auth::require_owner))
        .route_layer(axum::middleware::from_fn_with_state(
            guard.clone(),
            crate::auth::require_credential,
        ))
        .with_state(state)
}

fn owner_post(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", "Bearer owner-token-test-000000")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn owner_get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", "Bearer owner-token-test-000000")
        .body(Body::empty())
        .unwrap()
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

#[tokio::test]
async fn push_handlers_keep_expected_statuses() {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Arc::new(Outbox::open(&path).unwrap());
    let app = owner_push_app(outbox.clone()).await;

    let status = app
        .clone()
        .oneshot(owner_get("/api/push/status"))
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);

    let vapid = app
        .clone()
        .oneshot(owner_get("/api/push/vapid-public-key"))
        .await
        .unwrap();
    assert_eq!(vapid.status(), StatusCode::SERVICE_UNAVAILABLE);

    let upsert = app
        .clone()
        .oneshot(owner_post(
            "/api/push/subscriptions",
            &format!(
                r#"{{"endpoint":"https://fcm.googleapis.com/x","keys":{{"p256dh":"{TEST_P256DH}","auth":"{TEST_AUTH}"}},"label":"phone"}}"#
            ),
        ))
        .await
        .unwrap();
    assert_eq!(upsert.status(), StatusCode::OK);

    let test = app
        .clone()
        .oneshot(owner_post("/api/push/test", "{}"))
        .await
        .unwrap();
    assert_eq!(test.status(), StatusCode::OK);

    let removed = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/push/subscriptions")
                .header("authorization", "Bearer owner-token-test-000000")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"endpoint":"https://fcm.googleapis.com/x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
}

#[tokio::test]
async fn client_errors_requires_owner_and_accepts_bearer() {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Arc::new(Outbox::open(&path).unwrap());
    let app = owner_push_app(outbox).await;

    let anon = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/client-errors")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"message":"boom"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    let ok = app
        .oneshot(owner_post("/api/client-errors", r#"{"message":"boom"}"#))
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::NO_CONTENT);
}
