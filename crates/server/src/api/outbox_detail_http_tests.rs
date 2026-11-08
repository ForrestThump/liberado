//! Owner-authenticated outbox detail, complete, and snooze routes.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, SecondsFormat, Utc};
use liberado_outbox::{EnqueueOutcome, NewReminder, Outbox, OutboxSource, RES_COMPLETED, TASK_DUE};
use tower::ServiceExt;

use super::{complete_outbox, get_outbox_detail, snooze_outbox};
use crate::auth::{ApiAuth, ApiGuard};
use crate::state::AppState;

async fn owner_outbox_app(outbox: Arc<Outbox>, vault_path: std::path::PathBuf) -> Router {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(liberado_session_store::SessionStore::open(dir.path().join("sessions")).await);
    let mut state = AppState::for_test(sessions, None, dir.path().to_path_buf());
    state.outbox = Some(outbox);
    state.vault_path = vault_path.to_string_lossy().into_owned();
    let state = Arc::new(state);
    let guard = ApiGuard::with_auth(ApiAuth::from_values(Some("owner-token-test-000000"), &[]));
    Router::new()
        .route("/api/outbox/{id}", axum::routing::get(get_outbox_detail))
        .route(
            "/api/outbox/{id}/complete",
            axum::routing::post(complete_outbox),
        )
        .route(
            "/api/outbox/{id}/snooze",
            axum::routing::post(snooze_outbox),
        )
        .route_layer(axum::middleware::from_fn(crate::auth::require_owner))
        .route_layer(axum::middleware::from_fn_with_state(
            guard.clone(),
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

fn owner_post(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", "Bearer owner-token-test-000000")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
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
async fn get_outbox_detail_requires_owner_auth() {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Arc::new(Outbox::open(&path).unwrap());
    let app = owner_outbox_app(outbox, std::path::PathBuf::from("/tmp/v")).await;
    let response = app
        .clone()
        .oneshot(anon_get("/api/outbox/1"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_outbox_detail_returns_row_and_obsidian_url() {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Arc::new(Outbox::open(&path).unwrap());
    let now = Utc::now();
    outbox
        .enqueue(
            &NewReminder {
                idempotency_key: "k1".into(),
                channel: "webpush".into(),
                target: "sub:1".into(),
                title: "Due now: x".into(),
                body: "body".into(),
                due_at: now,
                catch_up: false,
                kind: Some(TASK_DUE.into()),
                source: Some(OutboxSource::VaultNote {
                    path: "Tasks/Inbox.md".into(),
                    label: "due".into(),
                    text: "Call dentist".into(),
                }),
            },
            now,
        )
        .unwrap();
    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("Tasks")).unwrap();
    std::fs::write(vault.path().join("Tasks/Inbox.md"), "- [ ] Call dentist\n").unwrap();
    let app = owner_outbox_app(outbox, vault.path().to_path_buf()).await;
    let response = app.oneshot(owner_get("/api/outbox/1")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["kind"], TASK_DUE);
    assert!(
        json["obsidian_url"]
            .as_str()
            .unwrap()
            .starts_with("obsidian://open?")
    );
}

#[tokio::test]
async fn complete_outbox_toggles_task_and_acks() {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Arc::new(Outbox::open(&path).unwrap());
    let now = Utc::now();
    outbox
        .enqueue(
            &NewReminder {
                idempotency_key: "k2".into(),
                channel: "webpush".into(),
                target: "sub:1".into(),
                title: "Due now: Call dentist".into(),
                body: "body".into(),
                due_at: now,
                catch_up: false,
                kind: Some(TASK_DUE.into()),
                source: Some(OutboxSource::VaultNote {
                    path: "Tasks/Inbox.md".into(),
                    label: "due".into(),
                    text: "Call dentist".into(),
                }),
            },
            now,
        )
        .unwrap();
    let vault = tempfile::tempdir().unwrap();
    let note = vault.path().join("Tasks/Inbox.md");
    std::fs::create_dir_all(note.parent().unwrap()).unwrap();
    std::fs::write(&note, "- [ ] Call dentist\n").unwrap();
    let app = owner_outbox_app(outbox.clone(), vault.path().to_path_buf()).await;
    let response = app
        .oneshot(owner_post("/api/outbox/1/complete", "{}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(outbox.acked(1).unwrap());
    let record = outbox.fetch_record(1).unwrap().unwrap();
    assert_eq!(record.resolution.as_deref(), Some(RES_COMPLETED));
    let text = std::fs::read_to_string(note).unwrap();
    assert!(text.contains("- [x] Call dentist"));
}

#[tokio::test]
async fn complete_outbox_refuses_ambiguous_task() {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Arc::new(Outbox::open(&path).unwrap());
    let now = Utc::now();
    outbox
        .enqueue(
            &NewReminder {
                idempotency_key: "k3".into(),
                channel: "webpush".into(),
                target: "sub:1".into(),
                title: "Due now: one".into(),
                body: "body".into(),
                due_at: now,
                catch_up: false,
                kind: Some(TASK_DUE.into()),
                source: Some(OutboxSource::VaultNote {
                    path: "Tasks/Inbox.md".into(),
                    label: "due".into(),
                    text: "one".into(),
                }),
            },
            now,
        )
        .unwrap();
    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("Tasks")).unwrap();
    std::fs::write(
        vault.path().join("Tasks/Inbox.md"),
        "- [ ] one\n- [ ] one\n",
    )
    .unwrap();
    let app = owner_outbox_app(outbox.clone(), vault.path().to_path_buf()).await;
    let response = app
        .oneshot(owner_post("/api/outbox/1/complete", "{}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(!outbox.acked(1).unwrap());
}

#[tokio::test]
async fn snooze_outbox_enqueues_followup_not_due_immediately() {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Arc::new(Outbox::open(&path).unwrap());
    let now = Utc::now();
    outbox
        .enqueue(
            &NewReminder {
                idempotency_key: "k4".into(),
                channel: "webpush".into(),
                target: "sub:1".into(),
                title: "Due now: x".into(),
                body: "body".into(),
                due_at: now,
                catch_up: false,
                kind: Some(TASK_DUE.into()),
                source: None,
            },
            now,
        )
        .unwrap();
    let app = owner_outbox_app(outbox.clone(), std::path::PathBuf::from("/tmp/v")).await;
    let response = app
        .oneshot(owner_post("/api/outbox/1/snooze", r#"{"minutes":15}"#))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let record = outbox.fetch_record(1).unwrap().unwrap();
    assert_eq!(record.resolution.as_deref(), Some("snoozed"));
    assert!(
        outbox.due_rows(now, 10).unwrap().is_empty(),
        "snoozed follow-up must not deliver before its due_at"
    );
    let fire = now + chrono::Duration::minutes(15) + chrono::Duration::seconds(1);
    let rows = outbox.due_rows(fire, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Snoozed: Due now: x");
}

fn enqueue_early(outbox: &Outbox, title: &str) -> (i64, chrono::DateTime<Utc>) {
    let now = Utc::now();
    let due = now - Duration::minutes(45);
    let EnqueueOutcome::Inserted(id) = outbox
        .enqueue(
            &NewReminder {
                idempotency_key: format!("early-{title}"),
                channel: "webpush".into(),
                target: "sub:1".into(),
                title: title.into(),
                body: "SECRET_BODY_DO_NOT_LOG".into(),
                due_at: due,
                catch_up: false,
                kind: Some(TASK_DUE.into()),
                source: Some(OutboxSource::VaultNote {
                    path: "Tasks/SECRETPATH.md".into(),
                    label: "due".into(),
                    text: title.into(),
                }),
            },
            now,
        )
        .unwrap()
    else {
        panic!("expected insert");
    };
    let stored = outbox.fetch_record(id).unwrap().unwrap().due_at;
    (id, stored)
}

fn assert_follow_up_uses_action_clock(
    outbox: &Outbox,
    id: i64,
    original: chrono::DateTime<Utc>,
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
) {
    let due = outbox
        .due_at_by_idempotency(&format!("snooze/{id}"))
        .unwrap()
        .unwrap();
    let lower = start + Duration::minutes(15) - Duration::seconds(2);
    let upper = end + Duration::minutes(15) + Duration::seconds(2);
    assert!(
        due >= lower && due <= upper,
        "follow-up due {due} not in [{lower}, {upper}]"
    );
    assert!(start.signed_duration_since(original) > Duration::minutes(30));
    assert_ne!(due, original + Duration::minutes(15));
    assert!(due > original + Duration::minutes(30));
}

#[tokio::test(flavor = "current_thread")]
async fn owner_snooze_follow_up_uses_action_time_and_log_hides_the_task() {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Arc::new(Outbox::open(&path).unwrap());
    let (id, original) = enqueue_early(&outbox, "SECRET_TITLE_DO_NOT_LOG");
    let app = owner_outbox_app(outbox.clone(), std::path::PathBuf::from("/tmp/v")).await;
    let logs = super::outbox_log::capture::LogCapture::new();
    let session = super::outbox_log::capture::Session::arm(logs);
    let start = Utc::now();
    let response = app
        .oneshot(owner_post(
            &format!("/api/outbox/{id}/snooze"),
            r#"{"minutes":15}"#,
        ))
        .await
        .unwrap();
    let end = Utc::now();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_follow_up_uses_action_clock(&outbox, id, original, start, end);
    let due = outbox
        .due_at_by_idempotency(&format!("snooze/{id}"))
        .unwrap()
        .unwrap();
    let text = session.text();
    let prior = original.to_rfc3339_opts(SecondsFormat::Secs, true);
    let new_due = due.to_rfc3339_opts(SecondsFormat::Secs, true);
    assert!(text.contains("message=outbox_action"), "{text}");
    assert!(text.contains("action=snooze"), "{text}");
    assert!(text.contains("via=detail"), "{text}");
    assert!(text.contains(&format!("row_id={id}")), "{text}");
    assert!(text.contains("minutes=15"), "{text}");
    assert!(text.contains("result=applied"), "{text}");
    assert!(text.contains(&format!("prior_due_at={prior}")), "{text}");
    assert!(text.contains(&format!("new_due_at={new_due}")), "{text}");
    assert!(text.contains("follow_up_row_id="), "{text}");
    assert!(!text.contains("SECRET"), "{text}");
}

#[tokio::test]
async fn complete_after_deliver_and_push_ack_succeeds() {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Arc::new(Outbox::open(&path).unwrap());
    let now = Utc::now();
    outbox
        .enqueue(
            &NewReminder {
                idempotency_key: "k5".into(),
                channel: "webpush".into(),
                target: "sub:1".into(),
                title: "Habit".into(),
                body: "body".into(),
                due_at: now,
                catch_up: false,
                kind: Some("habit".into()),
                source: None,
            },
            now,
        )
        .unwrap();
    let token = outbox.due_rows(now, 10).unwrap()[0].token.clone();
    outbox.lease(&[1], now).unwrap();
    outbox.mark_accepted(&[1], now).unwrap();
    outbox.ack(1, &token, now).unwrap();
    let app = owner_outbox_app(outbox.clone(), std::path::PathBuf::from("/tmp/v")).await;
    let response = app
        .oneshot(owner_post("/api/outbox/1/complete", "{}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let record = outbox.fetch_record(1).unwrap().unwrap();
    assert_eq!(record.resolution.as_deref(), Some("dismissed"));
}
