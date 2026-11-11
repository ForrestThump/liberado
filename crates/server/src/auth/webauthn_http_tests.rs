//! Passkey register/login against the real HTTP handlers (software authenticator).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::post;
use liberado_config::{ApiConfig, WebAuthnConfig};
use tower::ServiceExt;
use webauthn_authenticator_rs::prelude::{Url as AuthUrl, WebauthnAuthenticator};
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};

use crate::api::auth_login;
use crate::auth::webauthn_handlers::{
    passkey_login_finish, passkey_login_start, passkey_register_finish, passkey_register_start,
};
use crate::auth::{ApiAuth, ApiGuard};

const HUMAN: &str = "test-human-webauthn-00000000";
const ORIGIN: &str = "http://localhost:8080";

fn webauthn_api() -> ApiConfig {
    ApiConfig {
        webauthn: Some(WebAuthnConfig {
            rp_id: "localhost".into(),
            origins: vec![ORIGIN.into()],
            rp_name: "Liberado".into(),
        }),
        ..ApiConfig::default()
    }
}

fn app(guard: Arc<ApiGuard>) -> Router {
    Router::new()
        .route("/api/auth/login", post(auth_login))
        .route(
            "/api/auth/passkey/register/start",
            post(passkey_register_start),
        )
        .route(
            "/api/auth/passkey/register/finish",
            post(passkey_register_finish),
        )
        .route("/api/auth/passkey/login/start", post(passkey_login_start))
        .route("/api/auth/passkey/login/finish", post(passkey_login_finish))
        .with_state(guard)
}

fn merge_cookies(existing: &str, set_cookie: &[String]) -> String {
    let mut parts: Vec<String> = existing
        .split(';')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    for line in set_cookie {
        let name = line.split('=').next().unwrap_or("");
        parts.retain(|p| !p.starts_with(&format!("{name}=")));
        if let Some(pair) = line.split(';').next() {
            parts.push(pair.trim().to_string());
        }
    }
    parts.join("; ")
}

struct ResponseParts {
    status: StatusCode,
    body: String,
    cookies: Vec<String>,
}

async fn send(app: &Router, mut req: Request<Body>, jar: &mut String) -> ResponseParts {
    if !jar.is_empty() {
        req.headers_mut()
            .insert(header::COOKIE, jar.parse().unwrap());
    }
    req.headers_mut()
        .insert(header::ORIGIN, ORIGIN.parse().unwrap());
    let response = app.clone().oneshot(req).await.expect("router responds");
    let status = response.status();
    let cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !cookies.is_empty() {
        *jar = merge_cookies(jar, &cookies);
    }
    let bytes = axum::body::to_bytes(response.into_body(), 512 * 1024)
        .await
        .expect("body");
    ResponseParts {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
        cookies,
    }
}

#[tokio::test]
async fn software_authenticator_registers_and_signs_in() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard);
    let mut jar = String::new();

    let token_login = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"token":"{HUMAN}"}}"#)))
            .unwrap(),
        &mut jar,
    )
    .await;
    assert_eq!(token_login.status, StatusCode::OK, "{}", token_login.body);

    let reg_start = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/start")
            .body(Body::empty())
            .unwrap(),
        &mut jar,
    )
    .await;
    assert_eq!(reg_start.status, StatusCode::OK, "{}", reg_start.body);
    let options: serde_json::Value =
        serde_json::from_str(&reg_start.body).expect("register start json");
    let challenge: CreationChallengeResponse =
        serde_json::from_value(options.get("options").unwrap().clone()).expect("options shape");

    let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let auth_url = AuthUrl::parse(ORIGIN).expect("origin url");
    let cred = wa
        .do_registration(auth_url.clone(), challenge.clone())
        .expect("soft registration");
    let cred_json = serde_json::to_value(&cred).expect("credential json");

    let reg_finish = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/finish")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "credential": cred_json }).to_string(),
            ))
            .unwrap(),
        &mut jar,
    )
    .await;
    assert_eq!(reg_finish.status, StatusCode::OK, "{}", reg_finish.body);

    let login_start = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/start")
            .body(Body::empty())
            .unwrap(),
        &mut String::new(),
    )
    .await;
    assert_eq!(login_start.status, StatusCode::OK, "{}", login_start.body);
    let login_options: serde_json::Value =
        serde_json::from_str(&login_start.body).expect("login start json");
    let auth_challenge: RequestChallengeResponse =
        serde_json::from_value(login_options.get("options").unwrap().clone())
            .expect("login options shape");
    let mut login_jar = merge_cookies("", &login_start.cookies);

    let auth_cred = wa
        .do_authentication(auth_url, auth_challenge.clone())
        .expect("soft authentication");
    let auth_json = serde_json::to_value(&auth_cred).expect("auth json");

    let login_finish = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/finish")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "credential": auth_json }).to_string(),
            ))
            .unwrap(),
        &mut login_jar,
    )
    .await;
    assert_eq!(login_finish.status, StatusCode::OK, "{}", login_finish.body);
}

#[tokio::test]
async fn passkey_session_cannot_start_registration() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard.clone());

    let (session_id, _csrf) = guard
        .sessions
        .create(1, "passkey", Some("test"))
        .expect("session");
    let mut passkey_jar = format!("liberado_session={session_id}");

    let blocked = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/start")
            .body(Body::empty())
            .unwrap(),
        &mut passkey_jar,
    )
    .await;
    assert_eq!(blocked.status, StatusCode::FORBIDDEN);
    assert!(blocked.body.contains("emergency token"));
}

#[tokio::test]
async fn bearer_human_can_start_registration_without_cookie_session() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard);
    let mut jar = String::new();
    let resp = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/start")
            .header("authorization", format!("Bearer {HUMAN}"))
            .body(Body::empty())
            .unwrap(),
        &mut jar,
    )
    .await;
    assert_eq!(resp.status, StatusCode::OK, "{}", resp.body);
}

#[tokio::test]
async fn wrong_origin_is_rejected_for_passkey_login_start() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/passkey/login/start")
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("responds");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn register_finish_rejects_reused_ceremony_cookie_after_success() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard);
    let mut jar = String::new();
    let _ = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"token":"{HUMAN}"}}"#)))
            .unwrap(),
        &mut jar,
    )
    .await;
    let start = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/start")
            .body(Body::empty())
            .unwrap(),
        &mut jar,
    )
    .await;
    assert_eq!(start.status, StatusCode::OK);
    let start_json: serde_json::Value = serde_json::from_str(&start.body).unwrap();
    let options: CreationChallengeResponse =
        serde_json::from_value(start_json.get("options").unwrap().clone()).unwrap();
    let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let cred = wa
        .do_registration(AuthUrl::parse(ORIGIN).unwrap(), options)
        .unwrap();
    let cred_json = serde_json::to_value(&cred).unwrap();
    let body = serde_json::json!({ "credential": cred_json.clone() }).to_string();
    let mut ceremony_jar = jar.clone();

    let first = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/finish")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
        &mut jar,
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);

    let reuse = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/finish")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "credential": cred_json }).to_string(),
            ))
            .unwrap(),
        &mut ceremony_jar,
    )
    .await;
    assert_eq!(reuse.status, StatusCode::BAD_REQUEST, "{}", reuse.body);
}

#[tokio::test]
async fn login_finish_without_ceremony_cookie_is_bad_request() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard);
    let resp = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/finish")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"credential":{}}"#))
            .unwrap(),
        &mut String::new(),
    )
    .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST, "{}", resp.body);
    assert!(resp.body.contains("ceremony"), "{}", resp.body);
}

#[tokio::test]
async fn login_finish_rejects_malformed_credential_json() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard.clone());
    let mut jar = String::new();
    let _ = register_one_passkey(&app, &guard, &mut jar).await;
    let start = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/start")
            .body(Body::empty())
            .unwrap(),
        &mut String::new(),
    )
    .await;
    assert_eq!(start.status, StatusCode::OK);
    let mut ceremony_jar = merge_cookies("", &start.cookies);

    let bad = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/finish")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"credential":"not-an-object"}"#))
            .unwrap(),
        &mut ceremony_jar,
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    assert!(bad.body.contains("malformed"), "{}", bad.body);
}

#[tokio::test]
async fn login_finish_rejects_a_credential_that_does_not_match_the_challenge() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard.clone());
    let mut jar = String::new();
    let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
    register_one_passkey_with_authenticator(&app, &guard, &mut jar, &mut wa).await;
    let start = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/start")
            .body(Body::empty())
            .unwrap(),
        &mut String::new(),
    )
    .await;
    assert_eq!(start.status, StatusCode::OK);
    let login_options: serde_json::Value = serde_json::from_str(&start.body).unwrap();
    let auth_challenge: RequestChallengeResponse =
        serde_json::from_value(login_options.get("options").unwrap().clone()).unwrap();
    let mut ceremony_jar = merge_cookies("", &start.cookies);

    let auth_cred = wa
        .do_authentication(AuthUrl::parse(ORIGIN).unwrap(), auth_challenge)
        .expect("soft authentication");
    let mut auth_json = serde_json::to_value(&auth_cred).unwrap();
    auth_json["response"]["signature"] = serde_json::json!("AQID");

    let bad = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/finish")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "credential": auth_json }).to_string(),
            ))
            .unwrap(),
        &mut ceremony_jar,
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED, "{}", bad.body);
    assert!(bad.body.contains("sign-in failed"), "{}", bad.body);
}

#[tokio::test]
async fn login_finish_reports_unknown_passkey_when_row_was_removed() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard.clone());
    let mut jar = String::new();
    let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let passkey_id = register_one_passkey_with_authenticator(&app, &guard, &mut jar, &mut wa).await;
    let start = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/start")
            .body(Body::empty())
            .unwrap(),
        &mut String::new(),
    )
    .await;
    assert_eq!(start.status, StatusCode::OK);
    let login_options: serde_json::Value = serde_json::from_str(&start.body).unwrap();
    let auth_challenge: RequestChallengeResponse =
        serde_json::from_value(login_options.get("options").unwrap().clone()).unwrap();
    let mut ceremony_jar = merge_cookies("", &start.cookies);

    let auth_cred = wa
        .do_authentication(AuthUrl::parse(ORIGIN).unwrap(), auth_challenge)
        .expect("soft authentication");
    let auth_json = serde_json::to_value(&auth_cred).unwrap();

    guard
        .sessions
        .outbox()
        .delete_passkey(1, passkey_id)
        .expect("delete");

    let finish = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/finish")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "credential": auth_json }).to_string(),
            ))
            .unwrap(),
        &mut ceremony_jar,
    )
    .await;
    assert_eq!(finish.status, StatusCode::UNAUTHORIZED, "{}", finish.body);
    assert!(finish.body.contains("unknown passkey"), "{}", finish.body);
}

#[tokio::test]
async fn login_finish_when_passkeys_are_disabled_is_not_found() {
    let guard = ApiGuard::with_auth(ApiAuth::from_values(Some(HUMAN), &[]));
    let app = app(guard);
    let resp = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/finish")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"credential":{}}"#))
            .unwrap(),
        &mut String::new(),
    )
    .await;
    assert_eq!(resp.status, StatusCode::NOT_FOUND, "{}", resp.body);
}

#[tokio::test]
async fn register_finish_without_credential_is_unauthorized() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard);
    let resp = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/finish")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"credential":{}}"#))
            .unwrap(),
        &mut String::new(),
    )
    .await;
    assert_eq!(resp.status, StatusCode::UNAUTHORIZED, "{}", resp.body);
}

#[tokio::test]
async fn register_finish_from_passkey_session_is_forbidden() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard.clone());
    let (session_id, _csrf) = guard
        .sessions
        .create(1, "passkey", Some("test"))
        .expect("session");
    let mut passkey_jar = format!("liberado_session={session_id}");

    let resp = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/finish")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"credential":{}}"#))
            .unwrap(),
        &mut passkey_jar,
    )
    .await;
    assert_eq!(resp.status, StatusCode::FORBIDDEN, "{}", resp.body);
    assert!(resp.body.contains("emergency token"), "{}", resp.body);
}

#[tokio::test]
async fn register_finish_without_ceremony_cookie_is_bad_request() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard);
    let mut jar = String::new();
    let _ = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"token":"{HUMAN}"}}"#)))
            .unwrap(),
        &mut jar,
    )
    .await;

    let resp = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/finish")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"credential":{}}"#))
            .unwrap(),
        &mut jar,
    )
    .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST, "{}", resp.body);
}

#[tokio::test]
async fn register_finish_rejects_malformed_credential_json() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard);
    let mut jar = String::new();
    let _ = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"token":"{HUMAN}"}}"#)))
            .unwrap(),
        &mut jar,
    )
    .await;
    let start = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/start")
            .body(Body::empty())
            .unwrap(),
        &mut jar,
    )
    .await;
    assert_eq!(start.status, StatusCode::OK);
    let mut ceremony_jar = jar.clone();

    let bad = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/finish")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"credential":[]}"#))
            .unwrap(),
        &mut ceremony_jar,
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
}

#[tokio::test]
async fn register_finish_rejects_a_credential_that_does_not_match_the_challenge() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard);
    let mut jar = String::new();
    let _ = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"token":"{HUMAN}"}}"#)))
            .unwrap(),
        &mut jar,
    )
    .await;
    let start = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/start")
            .body(Body::empty())
            .unwrap(),
        &mut jar,
    )
    .await;
    assert_eq!(start.status, StatusCode::OK);
    let start_json: serde_json::Value = serde_json::from_str(&start.body).unwrap();
    let options: CreationChallengeResponse =
        serde_json::from_value(start_json.get("options").unwrap().clone()).unwrap();
    let mut ceremony_jar = jar.clone();
    let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
    let cred = wa
        .do_registration(AuthUrl::parse(ORIGIN).unwrap(), options)
        .expect("registration");
    let mut cred_json = serde_json::to_value(&cred).unwrap();
    cred_json["response"]["attestationObject"] = serde_json::json!("AQID");

    let bad = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/finish")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "credential": cred_json }).to_string(),
            ))
            .unwrap(),
        &mut ceremony_jar,
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    assert!(bad.body.contains("registration failed"), "{}", bad.body);
}

/// Register one passkey through the HTTP flow; returns the SQLite row id.
async fn register_one_passkey(app: &Router, guard: &ApiGuard, jar: &mut String) -> i64 {
    let mut wa = WebauthnAuthenticator::new(SoftPasskey::new(true));
    register_one_passkey_with_authenticator(app, guard, jar, &mut wa).await
}

async fn register_one_passkey_with_authenticator(
    app: &Router,
    guard: &ApiGuard,
    jar: &mut String,
    wa: &mut WebauthnAuthenticator<SoftPasskey>,
) -> i64 {
    let _ = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"token":"{HUMAN}"}}"#)))
            .unwrap(),
        jar,
    )
    .await;
    let reg_start = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/start")
            .body(Body::empty())
            .unwrap(),
        jar,
    )
    .await;
    assert_eq!(reg_start.status, StatusCode::OK, "{}", reg_start.body);
    let options: serde_json::Value = serde_json::from_str(&reg_start.body).unwrap();
    let challenge: CreationChallengeResponse =
        serde_json::from_value(options.get("options").unwrap().clone()).unwrap();
    let cred = wa
        .do_registration(AuthUrl::parse(ORIGIN).unwrap(), challenge)
        .expect("registration");
    let cred_json = serde_json::to_value(&cred).unwrap();
    let reg_finish = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/finish")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "credential": cred_json }).to_string(),
            ))
            .unwrap(),
        jar,
    )
    .await;
    assert_eq!(reg_finish.status, StatusCode::OK, "{}", reg_finish.body);
    guard
        .sessions
        .outbox()
        .list_passkeys(1)
        .expect("list")
        .into_iter()
        .next()
        .expect("one passkey")
        .id
}

async fn token_login(app: &Router, jar: &mut String) {
    let resp = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"token":"{HUMAN}"}}"#)))
            .unwrap(),
        jar,
    )
    .await;
    assert_eq!(resp.status, StatusCode::OK, "{}", resp.body);
}

#[tokio::test]
async fn register_start_json_exposes_public_key_challenge_for_webui() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard);
    let mut jar = String::new();
    token_login(&app, &mut jar).await;

    let reg_start = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/register/start")
            .body(Body::empty())
            .unwrap(),
        &mut jar,
    )
    .await;
    assert_eq!(reg_start.status, StatusCode::OK, "{}", reg_start.body);
    let body: serde_json::Value = serde_json::from_str(&reg_start.body).expect("json");
    let challenge = body
        .pointer("/options/publicKey/challenge")
        .expect("options.publicKey.challenge");
    assert!(
        challenge.is_string(),
        "challenge must be base64url string for webauthn.js, got {challenge}"
    );
    assert!(
        !challenge.as_str().unwrap().is_empty(),
        "challenge must not be empty"
    );
    serde_json::from_value::<CreationChallengeResponse>(body["options"].clone())
        .expect("options must deserialize as CreationChallengeResponse");
}

#[tokio::test]
async fn login_start_json_exposes_public_key_challenge_for_webui() {
    let guard = ApiGuard::with_api(ApiAuth::from_values(Some(HUMAN), &[]), webauthn_api());
    let app = app(guard.clone());
    let mut jar = String::new();
    token_login(&app, &mut jar).await;
    register_one_passkey(&app, &guard, &mut jar).await;

    let login_start = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkey/login/start")
            .body(Body::empty())
            .unwrap(),
        &mut String::new(),
    )
    .await;
    assert_eq!(login_start.status, StatusCode::OK, "{}", login_start.body);
    let body: serde_json::Value = serde_json::from_str(&login_start.body).expect("json");
    let challenge = body
        .pointer("/options/publicKey/challenge")
        .expect("options.publicKey.challenge");
    assert!(
        challenge.is_string(),
        "challenge must be base64url string for webauthn.js, got {challenge}"
    );
    assert!(
        !challenge.as_str().unwrap().is_empty(),
        "challenge must not be empty"
    );
    if let Some(allow) = body.pointer("/options/publicKey/allowCredentials") {
        assert!(
            allow.is_array(),
            "allowCredentials must be an array when present"
        );
        for entry in allow.as_array().unwrap() {
            let id = entry.get("id").expect("allowCredentials[].id");
            assert!(
                id.is_string(),
                "credential id must be base64url string, got {id}"
            );
        }
    }
    serde_json::from_value::<RequestChallengeResponse>(body["options"].clone())
        .expect("options must deserialize as RequestChallengeResponse");
}

#[test]
fn webui_browser_credential_json_deserializes_for_passkey_finish() {
    let register_json = include_str!("../../../webui/js/fixtures/register_credential.json");
    let reg: RegisterPublicKeyCredential =
        serde_json::from_str(register_json).expect("register credential json");
    assert_eq!(
        reg.extensions.cred_props.as_ref().and_then(|c| c.rk),
        Some(true)
    );
    assert!(
        reg.response
            .transports
            .as_ref()
            .is_some_and(|t| t.len() == 2),
        "expected internal and hybrid transports"
    );

    let assertion_json = include_str!("../../../webui/js/fixtures/assertion_credential.json");
    serde_json::from_str::<PublicKeyCredential>(assertion_json).expect("assertion credential json");

    let mut duplicate =
        serde_json::from_str::<serde_json::Value>(register_json).expect("register json value");
    let ext = duplicate
        .get("clientExtensionResults")
        .cloned()
        .expect("clientExtensionResults");
    duplicate
        .as_object_mut()
        .expect("object")
        .insert("extensions".into(), ext);
    assert!(
        serde_json::from_value::<RegisterPublicKeyCredential>(duplicate).is_err(),
        "duplicate clientExtensionResults and extensions keys must fail serde"
    );
}
