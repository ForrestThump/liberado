//! The request-time guard, exercised through a real router.
//!
//! Every token here is a literal generated for the test. None of them is a real credential.

use super::*;
use axum::{Router, body::Body, http::Request, routing::get};
use tower::ServiceExt;

const HUMAN: &str = "test-human-aaaaaaaaaaaaaaaa";
const AGENT: &str = "test-agent-bbbbbbbbbbbbbbbb";

async fn ok() -> &'static str {
    "ok"
}

fn guard(open: bool) -> Arc<ApiGuard> {
    let auth = match open {
        true => ApiAuth::default(),
        false => ApiAuth::from_values(Some(HUMAN), &[("coder", AGENT)]),
    };
    Arc::new(ApiGuard::new(auth, Vec::new()))
}

/// A router shaped like the daemon's: an authenticated route and an owner-only one behind the
/// same credential layer.
fn app(guard: &Arc<ApiGuard>) -> Router {
    let owner = Router::new()
        .route("/api/approvals/{id}/resolve", axum::routing::post(ok))
        .route_layer(axum::middleware::from_fn(require_owner));
    Router::new()
        .route("/api/status", get(ok))
        .route("/api/conversations", axum::routing::post(ok))
        .merge(owner)
        .route_layer(axum::middleware::from_fn_with_state(
            guard.clone(),
            require_credential,
        ))
}

struct Sent {
    status: StatusCode,
    body: String,
}

async fn send(app: &Router, request: Request<Body>) -> Sent {
    let response = app.clone().oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    Sent {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn get_request(path: &str) -> axum::http::request::Builder {
    Request::builder().method("GET").uri(path)
}

fn resolve_request() -> axum::http::request::Builder {
    Request::builder()
        .method("POST")
        .uri("/api/approvals/abc/resolve")
}

// ── Done-when 1: an unauthenticated request gets 401 ──────────────────────────

/// Simulates the other tailnet host in the review: a caller holding no credential at all.
#[tokio::test]
async fn an_unauthenticated_request_is_401() {
    let guard = guard(false);
    let sent = send(
        &app(&guard),
        get_request("/api/status").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED, "{}", sent.body);
    assert!(sent.body.contains("unauthenticated"), "{}", sent.body);
}

#[tokio::test]
async fn a_wrong_bearer_token_is_401_and_the_refusal_says_nothing_useful() {
    let guard = guard(false);
    let sent = send(
        &app(&guard),
        get_request("/api/status")
            .header("authorization", "Bearer test-wrong-cccccccccccccccc")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    assert!(
        !sent.body.contains("test-wrong"),
        "the refusal must not echo the presented token: {}",
        sent.body
    );
}

#[tokio::test]
async fn a_client_token_reaches_the_ordinary_api() {
    let guard = guard(false);
    let sent = send(
        &app(&guard),
        get_request("/api/status")
            .header("authorization", format!("Bearer {AGENT}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
}

#[tokio::test]
async fn the_bearer_scheme_is_matched_case_insensitively_and_nothing_else_is() {
    let guard = guard(false);
    for scheme in ["bearer", "BEARER", "Bearer"] {
        let sent = send(
            &app(&guard),
            get_request("/api/status")
                .header("authorization", format!("{scheme} {HUMAN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(sent.status, StatusCode::OK, "scheme {scheme}");
    }
    let basic = send(
        &app(&guard),
        get_request("/api/status")
            .header("authorization", format!("Basic {HUMAN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(basic.status, StatusCode::UNAUTHORIZED);
}

// ── Done-when 3: a coding run that curls resolve is refused ───────────────────

/// The coding shell in the review: a `curl` with no credential, and a `curl` carrying the
/// per-client token a coding run could plausibly have been given.
#[tokio::test]
async fn resolving_an_approval_without_a_credential_is_401() {
    let guard = guard(false);
    let sent = send(&app(&guard), resolve_request().body(Body::empty()).unwrap()).await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED, "{}", sent.body);
}

#[tokio::test]
async fn resolving_an_approval_with_a_client_token_is_403() {
    let guard = guard(false);
    let sent = send(
        &app(&guard),
        resolve_request()
            .header("authorization", format!("Bearer {AGENT}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::FORBIDDEN, "{}", sent.body);
    assert!(
        sent.body.contains("coder"),
        "names the client: {}",
        sent.body
    );
    assert!(sent.body.contains("owner-only"), "{}", sent.body);
}

#[tokio::test]
async fn resolving_an_approval_with_the_human_token_is_allowed() {
    let guard = guard(false);
    let sent = send(
        &app(&guard),
        resolve_request()
            .header("authorization", format!("Bearer {HUMAN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
}

// ── The browser session, and the CSRF check that goes with it ─────────────────

#[tokio::test]
async fn a_session_cookie_authenticates_a_read() {
    let guard = guard(false);
    let (id, _) = guard.sessions.create();
    let sent = send(
        &app(&guard),
        get_request("/api/status")
            .header("cookie", format!("liberado_session={id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
}

/// A cookie is the one credential a browser attaches without being asked, so it is the one
/// credential that needs a second factor the attacking page cannot supply.
#[tokio::test]
async fn a_cookie_authenticated_write_without_the_csrf_header_is_403() {
    let guard = guard(false);
    let (id, _) = guard.sessions.create();
    let sent = send(
        &app(&guard),
        Request::builder()
            .method("POST")
            .uri("/api/conversations")
            .header("cookie", format!("liberado_session={id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::FORBIDDEN, "{}", sent.body);
    assert!(sent.body.contains("X-Liberado-CSRF"), "{}", sent.body);
}

#[tokio::test]
async fn a_cookie_authenticated_write_with_the_csrf_header_is_allowed() {
    let guard = guard(false);
    let (id, csrf) = guard.sessions.create();
    let sent = send(
        &app(&guard),
        Request::builder()
            .method("POST")
            .uri("/api/conversations")
            .header("cookie", format!("liberado_session={id}"))
            .header("x-liberado-csrf", csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
}

#[tokio::test]
async fn another_sessions_csrf_token_does_not_work() {
    let guard = guard(false);
    let (id, _) = guard.sessions.create();
    let (_, other_csrf) = guard.sessions.create();
    let sent = send(
        &app(&guard),
        Request::builder()
            .method("POST")
            .uri("/api/conversations")
            .header("cookie", format!("liberado_session={id}"))
            .header("x-liberado-csrf", other_csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::FORBIDDEN, "{}", sent.body);
}

// ── Done-when 2 (server half): a write from a foreign origin is refused ───────

/// `SameSite=Strict` already stops the browser sending the cookie here. This is the belt to
/// that pair of braces, and it is what the test can observe without a browser.
#[tokio::test]
async fn a_cookie_authenticated_write_from_a_foreign_origin_is_403() {
    let guard = guard(false);
    let (id, csrf) = guard.sessions.create();
    let sent = send(
        &app(&guard),
        Request::builder()
            .method("POST")
            .uri("/api/conversations")
            .header("host", "127.0.0.1:4201")
            .header("origin", "https://evil.example")
            .header("cookie", format!("liberado_session={id}"))
            .header("x-liberado-csrf", csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::FORBIDDEN, "{}", sent.body);
    assert!(sent.body.contains("Origin"), "{}", sent.body);
}

#[tokio::test]
async fn a_write_from_our_own_origin_is_allowed() {
    let guard = guard(false);
    let (id, csrf) = guard.sessions.create();
    let sent = send(
        &app(&guard),
        Request::builder()
            .method("POST")
            .uri("/api/conversations")
            .header("host", "127.0.0.1:4201")
            .header("origin", "http://127.0.0.1:4201")
            .header("cookie", format!("liberado_session={id}"))
            .header("x-liberado-csrf", csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
}

#[tokio::test]
async fn a_listed_origin_is_allowed() {
    let auth = ApiAuth::from_values(Some(HUMAN), &[]);
    let guard = Arc::new(ApiGuard::new(
        auth,
        vec!["http://127.0.0.1:8080".to_string()],
    ));
    let (id, csrf) = guard.sessions.create();
    let sent = send(
        &app(&guard),
        Request::builder()
            .method("POST")
            .uri("/api/conversations")
            .header("host", "127.0.0.1:4201")
            .header("origin", "http://127.0.0.1:8080")
            .header("cookie", format!("liberado_session={id}"))
            .header("x-liberado-csrf", csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
}

/// A bearer token is never attached by a browser on its own, so it carries no CSRF risk and
/// gets no CSRF requirement. Demanding one would break every non-browser client for nothing.
#[tokio::test]
async fn a_bearer_authenticated_write_needs_no_csrf_header() {
    let guard = guard(false);
    let sent = send(
        &app(&guard),
        Request::builder()
            .method("POST")
            .uri("/api/conversations")
            .header("authorization", format!("Bearer {AGENT}"))
            .header("origin", "https://evil.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
}

#[tokio::test]
async fn a_revoked_session_cookie_stops_working() {
    let guard = guard(false);
    let (id, _) = guard.sessions.create();
    guard.sessions.revoke(&id);
    let sent = send(
        &app(&guard),
        get_request("/api/status")
            .header("cookie", format!("liberado_session={id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
}

// ── The loopback-only daemon ──────────────────────────────────────────────────

/// With no `[api]` table the daemon can only be bound to loopback, so the operating system is
/// the boundary and demanding a token nobody has would lock Forrest out of his own box.
#[tokio::test]
async fn a_daemon_with_no_credentials_serves_loopback_callers_as_the_owner() {
    let guard = guard(true);
    let sent = send(&app(&guard), resolve_request().body(Body::empty()).unwrap()).await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
}

/// A wiring mistake that mounts an owner-only route outside the credential layer must refuse,
/// not fall open. The whole band structure is one `route_layer` call away from being wrong.
#[tokio::test]
async fn an_owner_route_mounted_outside_the_credential_layer_refuses() {
    let unguarded = Router::new()
        .route("/api/approvals/{id}/resolve", axum::routing::post(ok))
        .route_layer(axum::middleware::from_fn(require_owner));
    let sent = send(&unguarded, resolve_request().body(Body::empty()).unwrap()).await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED, "{}", sent.body);
}
