//! HTTP contract for the weekly spending trigger.
//! These tests do not call a push service or the budget API.

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
use liberado_session_store::SessionStore;
use tower::ServiceExt;

use super::push_weekly;
use crate::state::AppState;

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
    Router::new()
        .route("/api/push/weekly", axum::routing::post(push_weekly))
        .with_state(Arc::new(state))
}

async fn call(app: Router, body: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/push/weekly")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn dry_run_renders_while_disabled_and_a_send_does_not_fetch() {
    let source = Arc::new(CountingSource {
        calls: AtomicUsize::new(0),
        error: None,
    });
    let (_dir, mut state) = open_state().await;
    state.spending = Some(Arc::clone(&source) as Arc<dyn SpendingSource>);
    let (status, body) = call(router(state), r#"{"dry_run":true,"force":true}"#).await;
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
    let (status, body) = call(router(state), r#"{"dry_run":false,"force":true}"#).await;
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
    let (status, body) = call(router(state), r#"{"dry_run":true}"#).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("budget API is not configured"));
    assert!(!body.contains("example-budget-key"));
    assert!(!body.contains("X-API-Key"));
}
