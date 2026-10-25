//! Sign-in, session read-back, and sign-out.
//!
//! Every token here is a literal generated for the test.

use super::*;
use crate::auth::ApiAuth;
use axum::{Router, body::Body, http::Request, routing::post};
use tower::ServiceExt;

const HUMAN: &str = "test-human-dddddddddddddddd";

fn app(guard: Arc<ApiGuard>) -> Router {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/session", axum::routing::get(session))
        .route("/api/health", axum::routing::get(health))
        .with_state(guard)
}

fn guard_with_human() -> Arc<ApiGuard> {
    Arc::new(ApiGuard::new(
        ApiAuth::from_values(Some(HUMAN), &[]),
        Vec::new(),
    ))
}

struct Sent {
    status: StatusCode,
    cookies: Vec<String>,
    body: String,
}

async fn send(app: &Router, request: Request<Body>) -> Sent {
    let response = app.clone().oneshot(request).await.expect("router responds");
    let status = response.status();
    let cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(str::to_string)
        .collect();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    Sent {
        status,
        cookies,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn login_request(token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
        .unwrap()
}

#[tokio::test]
async fn health_answers_without_a_credential_and_says_nothing_else() {
    let sent = send(
        &app(guard_with_human()),
        Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::OK);
    assert_eq!(sent.body, r#"{"ok":true}"#);
}

#[tokio::test]
async fn the_human_token_is_exchanged_for_a_strict_httponly_cookie() {
    let sent = send(&app(guard_with_human()), login_request(HUMAN)).await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);

    let session_cookie = sent
        .cookies
        .iter()
        .find(|c| c.starts_with("liberado_session="))
        .expect("a session cookie is set");
    assert!(session_cookie.contains("HttpOnly"), "{session_cookie}");
    assert!(
        session_cookie.contains("SameSite=Strict"),
        "a Lax cookie would be attached to a cross-site top-level POST: {session_cookie}"
    );
    assert!(
        !session_cookie.contains(HUMAN),
        "the cookie must not be the token itself: {session_cookie}"
    );

    let csrf_cookie = sent
        .cookies
        .iter()
        .find(|c| c.starts_with("liberado_csrf="))
        .expect("a readable CSRF cookie is set");
    assert!(
        !csrf_cookie.contains("HttpOnly"),
        "the WebUI has to read this one: {csrf_cookie}"
    );
    assert!(sent.body.contains("\"csrf\""), "{}", sent.body);
    assert!(
        sent.body.contains("\"authenticated\":true"),
        "{}",
        sent.body
    );
}

#[tokio::test]
async fn a_wrong_token_is_401_and_mints_nothing() {
    let sent = send(
        &app(guard_with_human()),
        login_request("test-wrong-eeeeeeeeeeeeeeee"),
    )
    .await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED, "{}", sent.body);
    assert!(sent.cookies.is_empty(), "{:?}", sent.cookies);
}

/// A client token authenticates the API. It is not the owner, so it cannot become a browser
/// session either — that would be the 403 on `approvals/*/resolve` handed back through a cookie.
#[tokio::test]
async fn a_client_token_cannot_be_exchanged_for_an_owner_session() {
    let guard = Arc::new(ApiGuard::new(
        ApiAuth::from_values(Some(HUMAN), &[("coder", "test-agent-ffffffffffffffff")]),
        Vec::new(),
    ));
    let sent = send(&app(guard), login_request("test-agent-ffffffffffffffff")).await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED, "{}", sent.body);
    assert!(sent.cookies.is_empty());
}

#[tokio::test]
async fn signing_in_with_no_human_credential_configured_is_404() {
    let guard = Arc::new(ApiGuard::new(ApiAuth::default(), Vec::new()));
    let sent = send(&app(guard), login_request(HUMAN)).await;
    assert_eq!(sent.status, StatusCode::NOT_FOUND, "{}", sent.body);
}

#[tokio::test]
async fn the_session_route_reports_whether_this_browser_is_signed_in() {
    let guard = guard_with_human();
    let app = app(guard.clone());

    let anonymous = send(
        &app,
        Request::builder()
            .uri("/api/auth/session")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        anonymous.body.contains("\"authenticated\":false"),
        "{}",
        anonymous.body
    );
    assert!(
        anonymous.body.contains("\"login_required\":true"),
        "{}",
        anonymous.body
    );

    let (id, csrf) = guard.sessions.create();
    let signed_in = send(
        &app,
        Request::builder()
            .uri("/api/auth/session")
            .header("cookie", format!("liberado_session={id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        signed_in.body.contains("\"authenticated\":true"),
        "{}",
        signed_in.body
    );
    assert!(signed_in.body.contains(&csrf), "{}", signed_in.body);
}

/// With no credential configured there is nothing to type, so the WebUI must not ask.
#[tokio::test]
async fn an_open_daemon_reports_that_no_login_is_required() {
    let guard = Arc::new(ApiGuard::new(ApiAuth::default(), Vec::new()));
    let sent = send(
        &app(guard),
        Request::builder()
            .uri("/api/auth/session")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        sent.body.contains("\"login_required\":false"),
        "{}",
        sent.body
    );
}

#[tokio::test]
async fn signing_out_revokes_the_session_and_clears_both_cookies() {
    let guard = guard_with_human();
    let (id, _) = guard.sessions.create();
    let sent = send(
        &app(guard.clone()),
        Request::builder()
            .method("POST")
            .uri("/api/auth/logout")
            .header("cookie", format!("liberado_session={id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(sent.status, StatusCode::OK);
    assert_eq!(
        guard.sessions.touch(&id),
        None,
        "sign-out must kill the session server-side, not only in this browser"
    );
    assert!(
        sent.cookies.iter().all(|c| c.contains("Max-Age=0")),
        "{:?}",
        sent.cookies
    );
}
