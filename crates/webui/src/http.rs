//! Authenticated HTTP to the daemon: session cookies, CSRF on writes, 401 → login gate.

use reqwest::{Client, RequestBuilder, Response, StatusCode};
use serde::Serialize;

pub const CSRF_HEADER: &str = "x-liberado-csrf";
const CSRF_COOKIE: &str = "liberado_csrf";

fn shared_client() -> Client {
    Client::new()
}

#[cfg(target_arch = "wasm32")]
fn csrf_token_from_cookie() -> Option<String> {
    use wasm_bindgen::JsCast;
    let window = web_sys::window()?;
    let doc = window
        .document()?
        .dyn_into::<web_sys::HtmlDocument>()
        .ok()?;
    let cookie = doc.cookie().ok()?;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix(&format!("{CSRF_COOKIE}=")) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn csrf_token_from_cookie() -> Option<String> {
    None
}

fn prepare(mut builder: RequestBuilder, mutating: bool) -> RequestBuilder {
    if mutating {
        if let Some(csrf) = csrf_token_from_cookie() {
            builder = builder.header(CSRF_HEADER, csrf);
        }
    }
    builder
}

/// `GET` with session cookies (same-origin).
pub async fn get(url: &str) -> Result<Response, reqwest::Error> {
    prepare(shared_client().get(url), false).send().await
}

/// `GET` with query parameters.
pub async fn get_query(url: &str, query: &[(&str, &str)]) -> Result<Response, reqwest::Error> {
    prepare(shared_client().get(url).query(query), false)
        .send()
        .await
}

/// `POST` with JSON body and CSRF when the browser holds a session.
pub async fn post_json(url: &str, body: &impl Serialize) -> Result<Response, reqwest::Error> {
    prepare(
        shared_client()
            .post(url)
            .header("content-type", "application/json"),
        true,
    )
    .json(body)
    .send()
    .await
}

/// `POST` with no body (cancel, discard, …).
pub async fn post_empty(url: &str) -> Result<Response, reqwest::Error> {
    prepare(shared_client().post(url), true).send().await
}

/// `PATCH` with JSON body.
pub async fn patch_json(url: &str, body: &impl Serialize) -> Result<Response, reqwest::Error> {
    prepare(
        shared_client()
            .patch(url)
            .header("content-type", "application/json"),
        true,
    )
    .json(body)
    .send()
    .await
}

/// `PUT` with JSON body.
pub async fn put_json(url: &str, body: &impl Serialize) -> Result<Response, reqwest::Error> {
    prepare(
        shared_client()
            .put(url)
            .header("content-type", "application/json"),
        true,
    )
    .json(body)
    .send()
    .await
}

/// `DELETE` with JSON body (push subscription removal).
pub async fn delete_json(url: &str, body: &impl Serialize) -> Result<Response, reqwest::Error> {
    prepare(
        shared_client()
            .delete(url)
            .header("content-type", "application/json"),
        true,
    )
    .json(body)
    .send()
    .await
}

/// `DELETE` (profile removal, …).
pub async fn delete(url: &str) -> Result<Response, reqwest::Error> {
    prepare(shared_client().delete(url), true).send().await
}

/// `POST /api/auth/login` — sets cookies; returns the session JSON.
pub async fn login(api_base: &str, token: &str) -> Result<Response, reqwest::Error> {
    let url = format!("{api_base}/api/auth/login");
    shared_client()
        .post(&url)
        .header("content-type", "application/json")
        .json(&serde_json::json!({ "token": token.trim() }))
        .send()
        .await
}

/// `GET /api/auth/session` — whether sign-in is required and whether this browser is in.
pub async fn session_status(api_base: &str) -> Result<Response, reqwest::Error> {
    get(&format!("{api_base}/api/auth/session")).await
}

/// True when the daemon refused the call for lack of a credential.
pub fn is_unauthenticated(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED
}
