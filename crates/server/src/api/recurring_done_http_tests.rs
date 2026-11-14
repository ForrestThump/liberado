//! Done on a 🔁 line leaves the note alone and logs the tap once.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use liberado_outbox::{
    EnqueueOutcome, NewReminder, Outbox, OutboxSource, PRECISION_TAP, RES_COMPLETED,
    SOURCE_LIBERADO, TASK_DUE,
};
use tower::ServiceExt;

use super::{complete_outbox, outbox_action_with_state};
use crate::auth::{ApiAuth, ApiGuard};
use crate::state::AppState;

async fn app(outbox: Arc<Outbox>, vault: std::path::PathBuf) -> Router {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(liberado_session_store::SessionStore::open(dir.path().join("sessions")).await);
    let mut state = AppState::for_test(sessions, None, dir.path().to_path_buf());
    state.outbox = Some(outbox);
    state.vault_path = vault.to_string_lossy().into_owned();
    let state = Arc::new(state);
    let guard = ApiGuard::with_auth(ApiAuth::from_values(Some("owner-token-test-000000"), &[]));
    let owner = Router::new()
        .route(
            "/api/outbox/{id}/complete",
            axum::routing::post(complete_outbox),
        )
        .route_layer(axum::middleware::from_fn(crate::auth::require_owner))
        .route_layer(axum::middleware::from_fn_with_state(
            guard,
            crate::auth::require_credential,
        ))
        .with_state(state.clone());
    let action = Router::new()
        .route(
            "/api/outbox/action",
            axum::routing::post(outbox_action_with_state),
        )
        .with_state(state);
    owner.merge(action)
}

fn note(vault: &std::path::Path, text: &str) {
    let path = vault.join("Tasks/Inbox.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn task(key: &str, text: &str) -> NewReminder {
    NewReminder {
        idempotency_key: key.into(),
        channel: "webpush".into(),
        target: "sub:1".into(),
        title: format!("Due now: {text}"),
        body: "body".into(),
        due_at: Utc::now(),
        catch_up: false,
        kind: Some(TASK_DUE.into()),
        source: Some(OutboxSource::VaultNote {
            path: "Tasks/Inbox.md".into(),
            label: "due".into(),
            text: text.into(),
        }),
    }
}

fn deliver(outbox: &Outbox, row: NewReminder) -> (i64, String) {
    let now = Utc::now();
    let EnqueueOutcome::Inserted(id) = outbox.enqueue(&row, now).unwrap() else {
        panic!("duplicate");
    };
    let token = outbox.due_rows(now, 10).unwrap()[0].token.clone();
    outbox.lease(&[id], now).unwrap();
    outbox.mark_accepted(&[id], now).unwrap();
    (id, token)
}

fn owner_post(uri: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", "Bearer owner-token-test-000000")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap()
}

fn post_json(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn detail_done_on_a_recurring_line_logs_one_tap_and_leaves_the_note() {
    let dir = tempfile::tempdir().unwrap();
    let outbox = Arc::new(Outbox::open(&dir.path().join("liberado.db")).unwrap());
    let (id, _) = deliver(&outbox, task("recur", "Call dentist"));
    let vault = tempfile::tempdir().unwrap();
    let original = "- [ ] Call dentist 🔁 every day\n";
    note(vault.path(), original);
    let app = app(outbox.clone(), vault.path().to_path_buf()).await;

    let first = app
        .clone()
        .oneshot(owner_post(&format!("/api/outbox/{id}/complete")))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    let path = vault.path().join("Tasks/Inbox.md");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let record = outbox.fetch_record(id).unwrap().unwrap();
    assert_eq!(record.resolution.as_deref(), Some(RES_COMPLETED));
    let rows = outbox.list_completions(None, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, SOURCE_LIBERADO);
    assert_eq!(rows[0].precision, PRECISION_TAP);
    assert_eq!(rows[0].outbox_id, Some(id));
    assert_eq!(rows[0].task_text, "Call dentist");

    let second = app
        .oneshot(owner_post(&format!("/api/outbox/{id}/complete")))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert_eq!(outbox.list_completions(None, 10).unwrap().len(), 1);
}

#[tokio::test]
async fn tray_done_on_a_recurring_line_also_logs_one_tap() {
    let dir = tempfile::tempdir().unwrap();
    let outbox = Arc::new(Outbox::open(&dir.path().join("liberado.db")).unwrap());
    let (id, token) = deliver(&outbox, task("tray", "Stretch"));
    let vault = tempfile::tempdir().unwrap();
    let original = "- [ ] Stretch 🔁 every weekday\n";
    note(vault.path(), original);
    let app = app(outbox.clone(), vault.path().to_path_buf()).await;
    let body = format!(r#"{{"id":{id},"token":"{token}","action":"done"}}"#);
    let first = app
        .clone()
        .oneshot(post_json("/api/outbox/action", &body))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(vault.path().join("Tasks/Inbox.md")).unwrap(),
        original
    );
    assert_eq!(outbox.list_completions(None, 10).unwrap().len(), 1);

    let second = app
        .oneshot(post_json("/api/outbox/action", &body))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(outbox.list_completions(None, 10).unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(vault.path().join("Tasks/Inbox.md")).unwrap(),
        original
    );
}

#[tokio::test]
async fn a_normal_done_checks_the_line_and_does_not_insert_the_log_row() {
    let dir = tempfile::tempdir().unwrap();
    let outbox = Arc::new(Outbox::open(&dir.path().join("liberado.db")).unwrap());
    let (id, _) = deliver(&outbox, task("plain", "Call dentist"));
    let vault = tempfile::tempdir().unwrap();
    note(vault.path(), "- [ ] Call dentist\n");
    let app = app(outbox.clone(), vault.path().to_path_buf()).await;
    let response = app
        .oneshot(owner_post(&format!("/api/outbox/{id}/complete")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let text = std::fs::read_to_string(vault.path().join("Tasks/Inbox.md")).unwrap();
    assert!(text.contains("- [x] Call dentist"), "{text}");
    assert!(outbox.list_completions(None, 10).unwrap().is_empty());
}
