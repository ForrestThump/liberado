//! Daemon origin for this client.
//!
//! `LIBERADO_SERVER` is the same variable the TUI and `liberado chat` already
//! use. There is no second base-URL variable and no credential in the URL.

use crate::error::DogfoodError;

/// Env var shared with the other Liberado HTTP clients.
pub const API_BASE_ENV: &str = "LIBERADO_SERVER";

/// Whole-request timeout, in seconds. Unset keeps this value.
pub const TIMEOUT_ENV: &str = "LIBERADO_DOGFOOD_TIMEOUT_SECS";

/// Loopback origin the daemon uses inside the container and on a local run.
pub const DEFAULT_API_BASE: &str = "http://127.0.0.1:4201";

/// A model turn can sit for minutes. Reads use the same ceiling so one client
/// covers both.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

const CONNECT_TIMEOUT_CAP: u64 = 10;

/// Resolve the daemon origin from an optional `LIBERADO_SERVER` value.
///
/// Blank means the loopback default. The result has no userinfo, no path, no
/// query, and no trailing slash. Userinfo is rejected so this client cannot
/// grow a second auth scheme beside the WebUI.
pub fn resolve_api_base(from_env: Option<&str>) -> Result<String, DogfoodError> {
    let raw = match from_env.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value,
        None => DEFAULT_API_BASE,
    };
    let url = reqwest::Url::parse(raw).map_err(|err| {
        DogfoodError::Invalid(format!("invalid {API_BASE_ENV} URL '{raw}': {err}"))
    })?;
    check_origin(&url)?;
    Ok(origin_string(&url))
}

/// Timeout from an optional `LIBERADO_DOGFOOD_TIMEOUT_SECS` value. Blank keeps
/// the default. Zero and non-integers are refused.
pub fn timeout_secs(from_env: Option<&str>) -> Result<u64, DogfoodError> {
    match from_env.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => parse_timeout(value),
        None => Ok(DEFAULT_TIMEOUT_SECS),
    }
}

/// Connect timeout is the request timeout, capped so a dead daemon fails fast
/// while a live chat turn may still use the longer ceiling.
pub fn connect_timeout(request_timeout: std::time::Duration) -> std::time::Duration {
    request_timeout.min(std::time::Duration::from_secs(CONNECT_TIMEOUT_CAP))
}

fn parse_timeout(value: &str) -> Result<u64, DogfoodError> {
    let seconds: u64 = value.parse().map_err(|_| {
        DogfoodError::Invalid(format!(
            "{TIMEOUT_ENV} must be a whole number of seconds, got {value:?}"
        ))
    })?;
    if seconds == 0 {
        return Err(DogfoodError::Invalid(format!(
            "{TIMEOUT_ENV} must be at least 1"
        )));
    }
    Ok(seconds)
}

fn check_origin(url: &reqwest::Url) -> Result<(), DogfoodError> {
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(DogfoodError::Invalid(format!(
                "{API_BASE_ENV} scheme must be http or https, got {other}"
            )));
        }
    }
    if url.host_str().is_none() {
        return Err(DogfoodError::Invalid(format!(
            "{API_BASE_ENV} is missing a host"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(DogfoodError::Invalid(format!(
            "{API_BASE_ENV} must not carry userinfo. The WebUI sends no credentials."
        )));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(DogfoodError::Invalid(format!(
            "{API_BASE_ENV} must be the daemon origin, with no query or fragment"
        )));
    }
    let path = url.path();
    if path != "/" && !path.is_empty() {
        return Err(DogfoodError::Invalid(format!(
            "{API_BASE_ENV} must be the daemon origin. The API is served at /api on that origin."
        )));
    }
    Ok(())
}

fn origin_string(url: &reqwest::Url) -> String {
    let mut origin = url.clone();
    origin.set_path("");
    origin.set_query(None);
    origin.set_fragment(None);
    let mut text = origin.to_string();
    if text.ends_with('/') {
        text.pop();
    }
    text
}

#[cfg(test)]
#[path = "base_url_tests.rs"]
mod tests;
