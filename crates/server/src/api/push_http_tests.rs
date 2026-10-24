//! HTTP contract for Web Push. These tests do not call a push service or the budget API.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use chrono::NaiveDate;
use liberado_daemon::{CategorySpend, SpendingSource};
use liberado_notify::{NewSubscription, PushHub};
use liberado_session_store::SessionStore;
use tower::ServiceExt;

use super::{
    delete_subscription, push_status, push_test, push_weekly, upsert_subscription, vapid_public_key,
};
use crate::state::AppState;

const PUBLIC: &str =
    "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
const PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
const P256DH: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
const ENDPOINT: &str = "https://push.example.test/fake-subscription";

struct CountingSource {
    calls: AtomicUsize,
    error: Option<&'static str>,
}

#[async_trait]
impl SpendingSource for CountingSource {
    async fn spending(
        &self,
        _start: NaiveDate,
        _end: NaiveDate,
        _exclude: &[String],
    ) -> Result<Vec<CategorySpend>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.error {
            return Err(error.to_string());
        }
        Ok(vec![CategorySpend {
            name: "Groceries".to_string(),
            total_cents: -21_200,
        }])
    }
}

async fn open_state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await;
    let state = AppState::for_test(Arc::new(store), None, dir.path().to_path_buf());
    (dir, state)
}

fn router(state: AppState) -> Router {
    let state = Arc::new(state);
    Router::new()
        .route("/api/push/status", axum::routing::get(push_status))
        .route(
            "/api/push/vapid-public-key",
            axum::routing::get(vapid_public_key),
        )
        .route(
            "/api/push/subscriptions",
            axum::routing::post(upsert_subscription).delete(delete_subscription),
        )
        .route("/api/push/test", axum::routing::post(push_test))
        .route("/api/push/weekly", axum::routing::post(push_weekly))
        .with_state(state)
}

async fn call(app: Router, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(match body {
            Some(text) => Body::from(text.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn subscribe_json() -> String {
    serde_json::json!({
        "endpoint": ENDPOINT,
        "keys": {"p256dh": P256DH, "auth": AUTH},
        "label": "iPhone"
    })
    .to_string()
}

#[tokio::test]
async fn status_and_the_public_key_stay_inert_without_keys() {
    let (_dir, state) = open_state().await;
    let (status, body) = call(router(state), "GET", "/api/push/status", None).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["vapid_configured"], false);
    assert_eq!(json["reminder_enabled"], false);
    assert_eq!(json["subscription_count"], 0);
    assert!(json["public_key"].is_null());

    let (_dir, state) = open_state().await;
    let (status, body) = call(router(state), "GET", "/api/push/vapid-public-key", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("Web Push keys are not configured"));
}

#[tokio::test]
async fn subscribe_and_delete_round_trip_on_a_local_file() {
    let (dir, mut state) = open_state().await;
    state.push = Arc::new(PushHub::at_path(dir.path().join("subs.json")));
    let (status, _) = call(
        router(state),
        "POST",
        "/api/push/subscriptions",
        Some(&subscribe_json()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let saved = std::fs::read_to_string(dir.path().join("subs.json")).unwrap();
    assert!(saved.contains(ENDPOINT));
    assert!(saved.contains("iPhone"));
    assert!(!saved.contains("example-budget-key"));

    let mut state = AppState::for_test(
        Arc::new(SessionStore::open(dir.path()).await),
        None,
        dir.path().to_path_buf(),
    );
    state.push = Arc::new(PushHub::at_path(dir.path().join("subs.json")));
    let (status, body) = call(router(state), "GET", "/api/push/status", None).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["subscription_count"], 1);

    let mut state = AppState::for_test(
        Arc::new(SessionStore::open(dir.path()).await),
        None,
        dir.path().to_path_buf(),
    );
    state.push = Arc::new(PushHub::at_path(dir.path().join("subs.json")));
    let (status, _) = call(
        router(state),
        "DELETE",
        "/api/push/subscriptions",
        Some(&format!(r#"{{"endpoint":"{ENDPOINT}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let saved = std::fs::read_to_string(dir.path().join("subs.json")).unwrap();
    assert!(!saved.contains(ENDPOINT));
}

#[tokio::test]
async fn a_test_push_refuses_missing_keys_an_empty_list_and_an_unknown_phone() {
    let (_dir, state) = open_state().await;
    let (status, body) = call(router(state), "POST", "/api/push/test", Some("{}")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("Web Push keys are not configured"));

    let (dir, mut state) = open_state().await;
    state.push = Arc::new(
        PushHub::with_keys(
            dir.path().join("subs.json"),
            PUBLIC,
            PRIVATE,
            "mailto:push@example.com",
        )
        .expect("rfc keys"),
    );
    let (status, body) = call(router(state), "POST", "/api/push/test", Some("{}")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("no web push subscriptions"));

    let (dir, mut state) = open_state().await;
    let hub = PushHub::with_keys(
        dir.path().join("subs.json"),
        PUBLIC,
        PRIVATE,
        "mailto:push@example.com",
    )
    .expect("rfc keys");
    hub.upsert(NewSubscription {
        endpoint: ENDPOINT.to_string(),
        p256dh: P256DH.to_string(),
        auth: AUTH.to_string(),
        label: "phone".to_string(),
    })
    .expect("save");
    state.push = Arc::new(hub);
    let (status, body) = call(
        router(state),
        "POST",
        "/api/push/test",
        Some(r#"{"endpoint":"https://push.example.test/missing"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn dry_run_renders_while_disabled_and_a_send_does_not_fetch() {
    let source = Arc::new(CountingSource {
        calls: AtomicUsize::new(0),
        error: None,
    });
    let (_dir, mut state) = open_state().await;
    state.spending = Some(Arc::clone(&source) as Arc<dyn SpendingSource>);
    let (status, body) = call(
        router(state),
        "POST",
        "/api/push/weekly",
        Some(r#"{"dry_run":true,"force":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("groceries $212 of $240 ($28 left)"), "{body}");
    assert!(!body.contains("example-budget-key"));
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);

    let source = Arc::new(CountingSource {
        calls: AtomicUsize::new(0),
        error: None,
    });
    let (_dir, mut state) = open_state().await;
    state.spending = Some(Arc::clone(&source) as Arc<dyn SpendingSource>);
    let (status, body) = call(
        router(state),
        "POST",
        "/api/push/weekly",
        Some(r#"{"dry_run":false,"force":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("weekly spending is off"));
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_missing_budget_client_is_503_and_hides_secrets() {
    let source = Arc::new(CountingSource {
        calls: AtomicUsize::new(0),
        error: Some("budget API is not configured"),
    });
    let (_dir, mut state) = open_state().await;
    state.spending = Some(source);
    let (status, body) = call(
        router(state),
        "POST",
        "/api/push/weekly",
        Some(r#"{"dry_run":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("budget API is not configured"));
    assert!(!body.contains("example-budget-key"));
    assert!(!body.contains("X-API-Key"));
}
