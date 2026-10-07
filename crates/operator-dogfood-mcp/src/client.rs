//! HTTP calls to the daemon. No `Authorization` header is set.
//!
//! Redirects are not followed. A 3xx is a failure, so a daemon response cannot
//! send this client off the operator origin onto another scheme.

use std::time::Duration;

use serde_json::Value;

use crate::base_url::{connect_timeout, resolve_api_base, timeout_secs};
use crate::error::DogfoodError;

const USER_AGENT: &str = "liberado-operator-dogfood-mcp/0.1.0";

/// One reqwest pool aimed at a single daemon origin.
#[derive(Clone)]
pub struct DaemonClient {
    http: reqwest::Client,
    base: String,
}

impl DaemonClient {
    /// Build a client for an origin that `resolve_api_base` already checked.
    pub fn new(base: String, timeout: Duration) -> Result<Self, DogfoodError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(connect_timeout(timeout))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .map_err(|err| DogfoodError::Daemon(format!("HTTP client: {err}")))?;
        Ok(Self { http, base })
    }

    /// Client used by the binary: `LIBERADO_SERVER` and the timeout env.
    pub fn from_process_env() -> Result<Self, DogfoodError> {
        let base = resolve_api_base(std::env::var(crate::base_url::API_BASE_ENV).ok().as_deref())?;
        let seconds = timeout_secs(std::env::var(crate::base_url::TIMEOUT_ENV).ok().as_deref())?;
        Self::new(base, Duration::from_secs(seconds))
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn get_json(&self, path: &str) -> Result<Value, DogfoodError> {
        self.exchange(reqwest::Method::GET, path, None).await
    }

    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value, DogfoodError> {
        self.exchange(reqwest::Method::POST, path, Some(body)).await
    }

    async fn exchange(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, DogfoodError> {
        let url = format!("{}{path}", self.base);
        let mut request = self.http.request(method.clone(), &url);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|err| DogfoodError::Daemon(format!("{method} {path} failed: {err}")))?;
        let status = response.status();
        let text = response.text().await.map_err(|err| {
            DogfoodError::Daemon(format!("{method} {path} body read failed: {err}"))
        })?;
        if !status.is_success() {
            return Err(failure(method.as_str(), path, status, &text));
        }
        parse_json(method.as_str(), path, &text)
    }
}

fn parse_json(method: &str, path: &str, text: &str) -> Result<Value, DogfoodError> {
    if text.trim().is_empty() {
        return Err(DogfoodError::Daemon(format!(
            "{method} {path} returned an empty body"
        )));
    }
    serde_json::from_str(text).map_err(|err| {
        DogfoodError::Daemon(format!(
            "{method} {path} was not JSON: {err}; body: {}",
            snippet(text)
        ))
    })
}

fn failure(method: &str, path: &str, status: reqwest::StatusCode, text: &str) -> DogfoodError {
    DogfoodError::Daemon(format!(
        "{method} {path} returned {status}: {}",
        error_detail(text)
    ))
}

fn error_detail(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| snippet(text).to_owned())
}

fn snippet(text: &str) -> &str {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(500) {
        Some((idx, _)) => &trimmed[..idx],
        None => trimmed,
    }
}
