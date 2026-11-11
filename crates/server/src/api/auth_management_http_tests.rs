//! Passkey rename/delete and auth-session revoke on the production router.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use liberado_session_store::SessionStore;
use tower::ServiceExt;

use crate::auth::sessions::CSRF_HEADER;
use crate::auth::{ApiAuth, ApiGuard};
use crate::state::AppState;

const HUMAN: &str = "test-human-mgmt-000000000000";
const AGENT: &str = "test-agent-mgmt-1111111111";

struct Harness {
    app: Router,
    guard: Arc<ApiGuard>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let state = Arc::new(AppState::for_test(sessions, None, dir.path().to_path_buf()));
    let guard = ApiGuard::with_auth(ApiAuth::from_values(Some(HUMAN), &[("coder", AGENT)]));
    let app = crate::build_app_router(&state, &guard);
    Harness {
        app,
        guard,
        _dir: dir,
    }
}

async fn call(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn bearer(method: &str, uri: &str, token: &str, body: Option<&str>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    match body {
        Some(b) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

fn browser_session(
    method: &str,
    uri: &str,
    session_id: &str,
    csrf: &str,
    body: Option<&str>,
) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:4201")
        .header("origin", "http://127.0.0.1:4201")
        .header("cookie", format!("liberado_session={session_id}"))
        .header(CSRF_HEADER, csrf);
    match body {
        Some(b) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

fn insert_passkey(guard: &ApiGuard, name: &str) -> i64 {
    guard
        .sessions
        .outbox()
        .insert_passkey(
            1,
            &format!("cred-{name}").into_bytes(),
            "{}",
            name,
            Utc::now(),
        )
        .expect("insert passkey")
}

#[tokio::test]
async fn passkey_management_routes_require_a_credential() {
    let h = harness().await;
    let id = insert_passkey(&h.guard, "a");
    for (method, uri) in [
        ("PATCH", format!("/api/auth/passkeys/{id}")),
        ("DELETE", format!("/api/auth/passkeys/{id}")),
        ("DELETE", "/api/auth/sessions/deadbeef".into()),
    ] {
        let (status, body) = call(
            &h.app,
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"x"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    }
}

#[tokio::test]
async fn passkey_and_session_mutations_reject_a_client_token() {
    let h = harness().await;
    let id = insert_passkey(&h.guard, "b");
    for (method, uri, body) in [
        (
            "PATCH",
            format!("/api/auth/passkeys/{id}"),
            Some(r#"{"name":"Renamed"}"#),
        ),
        ("DELETE", format!("/api/auth/passkeys/{id}"), None),
        ("DELETE", "/api/auth/sessions/abc".into(), None),
    ] {
        let (status, body) = call(&h.app, bearer(method, &uri, AGENT, body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
        assert!(body.contains("owner session"), "{body}");
    }
}

#[tokio::test]
async fn browser_session_mutations_require_csrf() {
    let h = harness().await;
    let id = insert_passkey(&h.guard, "c");
    let (session_id, _csrf) = h.guard.sessions.create(1, "token", None).unwrap();
    let (status, body) = call(
        &h.app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/auth/passkeys/{id}"))
            .header("cookie", format!("liberado_session={session_id}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name":"No CSRF"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("CSRF"), "{body}");
}

#[tokio::test]
async fn owner_renames_and_deletes_passkeys() {
    let h = harness().await;
    let id = insert_passkey(&h.guard, "old-name");

    let (status, body) = call(
        &h.app,
        bearer(
            "PATCH",
            &format!("/api/auth/passkeys/{id}"),
            HUMAN,
            Some(r#"{"name":"  New label  "}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let rows = h.guard.sessions.outbox().list_passkeys(1).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "New label");

    let (status, body) = call(
        &h.app,
        bearer("DELETE", &format!("/api/auth/passkeys/{id}"), HUMAN, None),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(
        h.guard
            .sessions
            .outbox()
            .list_passkeys(1)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn passkey_patch_and_delete_return_not_found_for_unknown_ids() {
    let h = harness().await;
    let (status, body) = call(
        &h.app,
        bearer(
            "PATCH",
            "/api/auth/passkeys/99999",
            HUMAN,
            Some(r#"{"name":"nope"}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body) = call(
        &h.app,
        bearer("DELETE", "/api/auth/passkeys/99999", HUMAN, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn passkey_rename_rejects_invalid_names() {
    let h = harness().await;
    let id = insert_passkey(&h.guard, "d");
    let (status, body) = call(
        &h.app,
        bearer(
            "PATCH",
            &format!("/api/auth/passkeys/{id}"),
            HUMAN,
            Some(r#"{"name":"   "}"#),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("1–80 characters"), "{body}");
}

#[tokio::test]
async fn owner_lists_and_revokes_auth_sessions() {
    let h = harness().await;
    let (current_id, csrf) = h
        .guard
        .sessions
        .create(1, "token", Some("browser-a"))
        .unwrap();
    let current = h.guard.sessions.touch(&current_id).unwrap();
    let (other_raw, _) = h
        .guard
        .sessions
        .outbox()
        .create_auth_session(1, "passkey", Some("browser-b"), Utc::now())
        .unwrap();
    let other = h
        .guard
        .sessions
        .outbox()
        .touch_auth_session(&other_raw, Utc::now())
        .unwrap();

    let (status, body) = call(
        &h.app,
        browser_session("GET", "/api/auth/sessions", &current_id, &csrf, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&other.public_id));
    assert!(body.contains(&format!("\"id\":\"{}\"", current.public_id)));
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).expect("session list json");
    assert_eq!(
        parsed
            .iter()
            .find(|row| row["id"] == current.public_id)
            .and_then(|row| row["current"].as_bool()),
        Some(true)
    );

    let (status, body) = call(
        &h.app,
        browser_session(
            "DELETE",
            &format!("/api/auth/sessions/{}", other.public_id),
            &current_id,
            &csrf,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(
        h.guard
            .sessions
            .outbox()
            .touch_auth_session(&other_raw, Utc::now())
            .is_none()
    );
    assert!(h.guard.sessions.touch(&current_id).is_some());
}

#[tokio::test]
async fn revoking_an_unknown_session_public_id_is_not_found() {
    let h = harness().await;
    let (status, body) = call(
        &h.app,
        bearer("DELETE", "/api/auth/sessions/00000000", HUMAN, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn owner_lists_passkeys_with_bearer_token() {
    let h = harness().await;
    insert_passkey(&h.guard, "listed");
    let (status, body) = call(&h.app, bearer("GET", "/api/auth/passkeys", HUMAN, None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("listed"));
}
