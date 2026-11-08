//! Token-scoped tray actions: same transitions as owner complete and snooze,
//! refused without the row's push token, and still closed on the owner routes.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, SecondsFormat, Utc};
use liberado_outbox::{EnqueueOutcome, NewReminder, Outbox, OutboxSource, RES_COMPLETED, TASK_DUE};
use tower::ServiceExt;

use super::{complete_outbox, get_outbox_detail, outbox_action_with_state, snooze_outbox};
use crate::auth::{ApiAuth, ApiGuard};
use crate::state::AppState;

async fn action_app(outbox: Arc<Outbox>, vault_path: PathBuf) -> Router {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(liberado_session_store::SessionStore::open(dir.path().join("sessions")).await);
    let mut state = AppState::for_test(sessions, None, dir.path().to_path_buf());
    state.outbox = Some(outbox);
    state.vault_path = vault_path.to_string_lossy().into_owned();
    let state = Arc::new(state);
    let guard = ApiGuard::with_auth(ApiAuth::from_values(Some("owner-token-test-000000"), &[]));
    let owner = Router::new()
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
        .with_state(state.clone());
    let action = Router::new()
        .route(
            "/api/outbox/action",
            axum::routing::post(outbox_action_with_state),
        )
        .with_state(state);
    owner.merge(action)
}

fn open_outbox() -> Arc<Outbox> {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    Arc::new(Outbox::open(&path).unwrap())
}

fn post_json(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
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

struct Delivered {
    id: i64,
    token: String,
}

fn deliver_row(outbox: &Outbox, row: NewReminder) -> Delivered {
    let now = Utc::now();
    let id = match outbox.enqueue(&row, now).unwrap() {
        liberado_outbox::EnqueueOutcome::Inserted(id) => id,
        liberado_outbox::EnqueueOutcome::Duplicate => panic!("duplicate row"),
    };
    let token = outbox
        .due_rows(now, 10)
        .unwrap()
        .into_iter()
        .find(|open| open.id == id)
        .unwrap()
        .token;
    outbox.lease(&[id], now).unwrap();
    outbox.mark_accepted(&[id], now).unwrap();
    Delivered { id, token }
}

fn vault_task(key: &str, text: &str) -> NewReminder {
    let now = Utc::now();
    NewReminder {
        idempotency_key: key.into(),
        channel: "webpush".into(),
        target: "sub:1".into(),
        title: format!("Due now: {text}"),
        body: "body".into(),
        due_at: now,
        catch_up: false,
        kind: Some(TASK_DUE.into()),
        source: Some(OutboxSource::VaultNote {
            path: "Tasks/Inbox.md".into(),
            label: "due".into(),
            text: text.into(),
        }),
    }
}

fn write_note(vault: &std::path::Path, text: &str) {
    let note = vault.join("Tasks/Inbox.md");
    std::fs::create_dir_all(note.parent().unwrap()).unwrap();
    std::fs::write(note, text).unwrap();
}

async fn json_body(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn action_done_completes_the_task_and_detail_shows_completed() {
    let outbox = open_outbox();
    let delivered = deliver_row(&outbox, vault_task("done", "Call dentist"));
    let vault = tempfile::tempdir().unwrap();
    write_note(vault.path(), "- [ ] Call dentist\n");
    let app = action_app(outbox.clone(), vault.path().to_path_buf()).await;

    let anon_complete = app
        .clone()
        .oneshot(post_json("/api/outbox/1/complete", "{}"))
        .await
        .unwrap();
    assert_eq!(anon_complete.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(post_json(
            "/api/outbox/action",
            &format!(
                r#"{{"id":{},"token":"{}","action":"done"}}"#,
                delivered.id, delivered.token
            ),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let notice = json_body(response).await;
    assert_eq!(notice["notice"], "Done: Due now: Call dentist");

    let record = outbox.fetch_record(delivered.id).unwrap().unwrap();
    assert_eq!(record.resolution.as_deref(), Some(RES_COMPLETED));
    assert!(outbox.acked(delivered.id).unwrap());
    let note = std::fs::read_to_string(vault.path().join("Tasks/Inbox.md")).unwrap();
    assert!(note.contains("- [x] Call dentist"), "{note}");

    let detail = app
        .oneshot(owner_get(&format!("/api/outbox/{}", delivered.id)))
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail = json_body(detail).await;
    assert_eq!(detail["resolution"], "completed");
    assert!(detail["acked"].as_bool().unwrap());
}

#[tokio::test]
async fn action_snooze_matches_owner_snooze_and_detail_shows_it() {
    let outbox = open_outbox();
    let delivered = deliver_row(&outbox, vault_task("snooze", "Call dentist"));
    let app = action_app(outbox.clone(), PathBuf::from("/tmp/v")).await;

    let response = app
        .clone()
        .oneshot(post_json(
            "/api/outbox/action",
            &format!(
                r#"{{"id":{},"token":"{}","action":"snooze"}}"#,
                delivered.id, delivered.token
            ),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let notice = json_body(response).await["notice"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(notice.starts_with("Snoozed until "), "{notice}");

    let record = outbox.fetch_record(delivered.id).unwrap().unwrap();
    assert_eq!(record.resolution.as_deref(), Some("snoozed"));
    let after = Utc::now();
    assert!(
        outbox.due_rows(after, 10).unwrap().is_empty(),
        "snoozed follow-up must not deliver before its due_at"
    );
    let too_soon = after + Duration::minutes(14);
    assert!(outbox.due_rows(too_soon, 10).unwrap().is_empty());
    let fire = after + Duration::minutes(15) + Duration::seconds(2);
    let rows = outbox.due_rows(fire, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Snoozed: Due now: Call dentist");

    let detail = app
        .oneshot(owner_get(&format!("/api/outbox/{}", delivered.id)))
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail = json_body(detail).await;
    assert_eq!(detail["resolution"], "snoozed");
    let until = detail["snooze_until_local"].as_str().unwrap();
    assert!(notice.contains(until), "{notice} vs {until}");
}

#[tokio::test]
async fn wrong_token_and_unknown_action_do_not_change_the_row() {
    let outbox = open_outbox();
    let delivered = deliver_row(&outbox, vault_task("guard", "Call dentist"));
    let other = deliver_row(
        &outbox,
        NewReminder {
            idempotency_key: "other".into(),
            ..vault_task("other", "Other task")
        },
    );
    let vault = tempfile::tempdir().unwrap();
    write_note(vault.path(), "- [ ] Call dentist\n- [ ] Other task\n");
    let app = action_app(outbox.clone(), vault.path().to_path_buf()).await;

    let wrong = app
        .clone()
        .oneshot(post_json(
            "/api/outbox/action",
            &format!(
                r#"{{"id":{},"token":"not-the-token","action":"done"}}"#,
                delivered.id
            ),
        ))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::FORBIDDEN);
    assert!(
        outbox
            .fetch_record(delivered.id)
            .unwrap()
            .unwrap()
            .resolution
            .is_none()
    );

    let mismatched = app
        .clone()
        .oneshot(post_json(
            "/api/outbox/action",
            &format!(
                r#"{{"id":{},"token":"{}","action":"done"}}"#,
                other.id, delivered.token
            ),
        ))
        .await
        .unwrap();
    assert_eq!(mismatched.status(), StatusCode::FORBIDDEN);
    assert!(
        outbox
            .fetch_record(other.id)
            .unwrap()
            .unwrap()
            .resolution
            .is_none()
    );

    let unknown = app
        .oneshot(post_json(
            "/api/outbox/action",
            &format!(
                r#"{{"id":{},"token":"{}","action":"approve"}}"#,
                delivered.id, delivered.token
            ),
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    assert!(
        outbox
            .fetch_record(delivered.id)
            .unwrap()
            .unwrap()
            .resolution
            .is_none()
    );
    let note = std::fs::read_to_string(vault.path().join("Tasks/Inbox.md")).unwrap();
    assert!(note.contains("- [ ] Call dentist"), "{note}");
    assert!(note.contains("- [ ] Other task"), "{note}");
}

#[tokio::test]
async fn already_resolved_is_409_and_unsupported_kind_is_400() {
    let outbox = open_outbox();
    let delivered = deliver_row(&outbox, vault_task("twice", "Call dentist"));
    let habit = deliver_row(
        &outbox,
        NewReminder {
            idempotency_key: "habit".into(),
            title: "Habit".into(),
            kind: Some("habit".into()),
            source: None,
            ..vault_task("habit-base", "ignored")
        },
    );
    let vault = tempfile::tempdir().unwrap();
    write_note(vault.path(), "- [ ] Call dentist\n");
    let app = action_app(outbox.clone(), vault.path().to_path_buf()).await;
    let body = format!(
        r#"{{"id":{},"token":"{}","action":"done"}}"#,
        delivered.id, delivered.token
    );
    let first = app
        .clone()
        .oneshot(post_json("/api/outbox/action", &body))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let second = app
        .clone()
        .oneshot(post_json("/api/outbox/action", &body))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
    let note = std::fs::read_to_string(vault.path().join("Tasks/Inbox.md")).unwrap();
    assert_eq!(note.matches("- [x] Call dentist").count(), 1, "{note}");

    let habit_body = format!(
        r#"{{"id":{},"token":"{}","action":"snooze","minutes":15}}"#,
        habit.id, habit.token
    );
    let refused = app
        .oneshot(post_json("/api/outbox/action", &habit_body))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(
        outbox
            .fetch_record(habit.id)
            .unwrap()
            .unwrap()
            .resolution
            .is_none()
    );
    assert!(
        outbox
            .due_rows(Utc::now() + Duration::hours(1), 10)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn owner_detail_still_requires_auth_on_the_production_shape() {
    let outbox = open_outbox();
    let _delivered = deliver_row(&outbox, vault_task("auth", "Call dentist"));
    let app = action_app(outbox, PathBuf::from("/tmp/v")).await;
    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/outbox/1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::UNAUTHORIZED);
    let snooze = app
        .oneshot(post_json("/api/outbox/1/snooze", r#"{"minutes":15}"#))
        .await
        .unwrap();
    assert_eq!(snooze.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn tray_snooze_follow_up_uses_action_time_and_log_hides_the_task() {
    let outbox = open_outbox();
    let now = Utc::now();
    let due = now - Duration::minutes(45);
    let EnqueueOutcome::Inserted(id) = outbox
        .enqueue(
            &NewReminder {
                idempotency_key: "tray-clock".into(),
                channel: "webpush".into(),
                target: "sub:1".into(),
                title: "SECRET_TITLE_DO_NOT_LOG".into(),
                body: "SECRET_BODY_DO_NOT_LOG".into(),
                due_at: due,
                catch_up: false,
                kind: Some(TASK_DUE.into()),
                source: Some(OutboxSource::VaultNote {
                    path: "Tasks/SECRETPATH.md".into(),
                    label: "due".into(),
                    text: "SECRET_TITLE_DO_NOT_LOG".into(),
                }),
            },
            now,
        )
        .unwrap()
    else {
        panic!("expected insert");
    };
    let original = outbox.fetch_record(id).unwrap().unwrap().due_at;
    let token = outbox
        .due_rows(Utc::now(), 10)
        .unwrap()
        .into_iter()
        .find(|row| row.id == id)
        .unwrap()
        .token;
    let app = action_app(outbox.clone(), PathBuf::from("/tmp/v")).await;
    let logs = super::outbox_log::capture::LogCapture::new();
    let session = super::outbox_log::capture::Session::arm(logs);
    let start = Utc::now();
    let response = app
        .oneshot(post_json(
            "/api/outbox/action",
            &format!(r#"{{"id":{id},"token":"{token}","action":"snooze","minutes":15}}"#),
        ))
        .await
        .unwrap();
    let end = Utc::now();
    assert_eq!(response.status(), StatusCode::OK);
    let follow = outbox
        .due_at_by_idempotency(&format!("snooze/{id}"))
        .unwrap()
        .unwrap();
    let lower = start + Duration::minutes(15) - Duration::seconds(2);
    let upper = end + Duration::minutes(15) + Duration::seconds(2);
    assert!(
        follow >= lower && follow <= upper,
        "follow-up due {follow} not in [{lower}, {upper}]"
    );
    assert!(start.signed_duration_since(original) > Duration::minutes(30));
    assert_ne!(follow, original + Duration::minutes(15));
    let text = session.text();
    let prior = original.to_rfc3339_opts(SecondsFormat::Secs, true);
    let new_due = follow.to_rfc3339_opts(SecondsFormat::Secs, true);
    assert!(text.contains("action=snooze"), "{text}");
    assert!(text.contains("via=tray"), "{text}");
    assert!(text.contains(&format!("row_id={id}")), "{text}");
    assert!(text.contains("minutes=15"), "{text}");
    assert!(text.contains("result=applied"), "{text}");
    assert!(text.contains(&format!("prior_due_at={prior}")), "{text}");
    assert!(text.contains(&format!("new_due_at={new_due}")), "{text}");
    assert!(text.contains("follow_up_row_id="), "{text}");
    assert!(!text.contains("SECRET"), "{text}");
    assert!(!text.contains(&token), "{text}");
}
