//! The production router's three authentication bands, checked on the real routes.
//!
//! `auth::guard` proves the middleware behaves. This file proves the daemon's own routes are
//! mounted in the bands the design says they are — the mistake that costs nothing to make and
//! everything to miss is a route added below the wrong `route_layer`, and only a test over
//! `build_app_router` can see it.
//!
//! Tokens here are literals generated for the test.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use liberado_session_store::SessionStore;
use tower::ServiceExt;

use crate::auth::{ApiAuth, ApiGuard};
use crate::state::AppState;

const HUMAN: &str = "test-human-router-0000000000";
const AGENT: &str = "test-agent-router-1111111111";

struct Harness {
    app: Router,
    guard: Arc<ApiGuard>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let state = Arc::new(AppState::for_test(sessions, None, dir.path().to_path_buf()));
    let guard = Arc::new(ApiGuard::new(
        ApiAuth::from_values(Some(HUMAN), &[("coder", AGENT)]),
        Vec::new(),
    ));
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

fn bare(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn with_token(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap()
}

/// Every API route that needs a credential, one per shape of thing the daemon exposes.
///
/// Not exhaustive by design — a list that has to be edited for every new route becomes a list
/// nobody edits. These cover status, the conversation surface, approvals, goals, and the
/// authority routes.
const AUTHENTICATED_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/status"),
    ("GET", "/api/models"),
    ("GET", "/api/catalog"),
    ("GET", "/api/vault"),
    ("GET", "/api/library"),
    ("GET", "/api/conversations"),
    ("GET", "/api/approvals/pending"),
    ("GET", "/api/profiles"),
    ("GET", "/api/sessions"),
    ("GET", "/api/goals"),
    ("GET", "/api/schedules"),
    ("POST", "/api/chat"),
    ("POST", "/api/goals"),
    ("POST", "/api/approvals/some-id/resolve"),
    ("POST", "/api/conversations/some-id/profile"),
    ("POST", "/api/models/select"),
    ("POST", "/api/mcp/reload"),
    ("PUT", "/api/plugins/some-plugin"),
    ("PATCH", "/api/profiles/some-profile"),
];

/// Done-when 1, on the real router: another tailnet host holds no credential, so every API
/// route answers 401 — including the ones that only read.
#[tokio::test]
async fn every_api_route_refuses_an_unauthenticated_caller() {
    let h = harness().await;
    for (method, uri) in AUTHENTICATED_ROUTES {
        let (status, body) = call(&h.app, bare(method, uri)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} answered {status} to a caller with no credential: {body}"
        );
    }
}

/// Done-when 3, on the real router: the credential a coding run could plausibly hold reaches
/// the working API and is refused by everything that decides.
#[tokio::test]
async fn a_client_token_is_refused_by_every_owner_only_route() {
    let h = harness().await;
    let owner_only = [
        ("POST", "/api/approvals/some-id/resolve"),
        ("POST", "/api/conversations/some-id/profile"),
        ("POST", "/api/models/select"),
        ("POST", "/api/mcp/reload"),
        ("PUT", "/api/plugins/some-plugin"),
        ("PATCH", "/api/profiles/some-profile"),
        ("DELETE", "/api/profiles/some-profile"),
    ];
    for (method, uri) in owner_only {
        let (status, body) = call(&h.app, with_token(method, uri, AGENT)).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {uri} let a client token through: {body}"
        );
        assert!(body.contains("owner-only"), "{method} {uri}: {body}");
    }
}

/// The other half of the same boundary: a client token has to keep working for the routes a
/// client exists to call, or the lockdown is just an outage.
#[tokio::test]
async fn a_client_token_still_reaches_the_read_and_work_routes() {
    let h = harness().await;
    for (method, uri) in [("GET", "/api/status"), ("GET", "/api/conversations")] {
        let (status, body) = call(&h.app, with_token(method, uri, AGENT)).await;
        assert_ne!(status, StatusCode::UNAUTHORIZED, "{method} {uri}: {body}");
        assert_ne!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
    }
}

/// The owner reaches the owner-only routes. A 404 or a 503 from the handler is a pass: the
/// question here is whether the request got past the guard, not whether the id exists.
#[tokio::test]
async fn the_human_token_passes_the_owner_gate() {
    let h = harness().await;
    for (method, uri) in [
        ("POST", "/api/approvals/some-id/resolve"),
        ("POST", "/api/models/select"),
    ] {
        let (status, body) = call(&h.app, with_token(method, uri, HUMAN)).await;
        assert_ne!(status, StatusCode::UNAUTHORIZED, "{method} {uri}: {body}");
        assert_ne!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
    }
}

#[tokio::test]
async fn health_and_the_sign_in_routes_need_no_credential() {
    let h = harness().await;
    let (status, body) = call(&h.app, bare("GET", "/api/health")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, r#"{"ok":true}"#);

    let (status, _) = call(&h.app, bare("GET", "/api/auth/session")).await;
    assert_eq!(status, StatusCode::OK);
}

/// Done-when 2, server half. A cross-origin `fetch` fails because the daemon sends no
/// `Access-Control-Allow-Origin` at all; `CorsLayer::permissive()` used to send `*`.
#[tokio::test]
async fn the_daemon_sends_no_cors_header_to_a_foreign_origin() {
    let h = harness().await;
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/status")
                .header("origin", "https://evil.example")
                .header("authorization", format!("Bearer {HUMAN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "a foreign origin must not be told it may read this response"
    );
}

/// The WebUI's own path, end to end through the production router: sign in, then use the
/// cookie and the CSRF token to resolve an approval.
#[tokio::test]
async fn the_browser_session_reaches_an_owner_only_route() {
    let h = harness().await;
    let (id, csrf) = h.guard.sessions.create();
    let (status, body) = call(
        &h.app,
        Request::builder()
            .method("POST")
            .uri("/api/approvals/some-id/resolve")
            .header("host", "127.0.0.1:4201")
            .header("origin", "http://127.0.0.1:4201")
            .header("cookie", format!("liberado_session={id}"))
            .header("x-liberado-csrf", csrf)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"decision":"approve"}"#))
            .unwrap(),
    )
    .await;
    assert_ne!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_ne!(status, StatusCode::FORBIDDEN, "{body}");
}

/// The static bundle must load before anyone can sign in, so the credential layer has to stay
/// off the fallback. A `route_layer` that became a `layer` would break exactly this.
#[tokio::test]
async fn the_static_frontend_is_reachable_before_sign_in() {
    let h = harness().await;
    let (status, _) = call(&h.app, bare("GET", "/index.html")).await;
    assert_ne!(
        status,
        StatusCode::UNAUTHORIZED,
        "the sign-in page cannot require being signed in"
    );
}
