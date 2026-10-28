//! Daemon goal HTTP for `liberado shepherd`.
//!
//! The active-goal count is a `Result`. Callers must not treat a failure as zero
//! open goals: zero means a free slot, and a free slot starts work. Auth failures
//! (HTTP 401 and 403), other non-2xx responses, transport errors, and JSON that
//! is not a goal list are all failures. The error text names
//! [`chat_client_contract::api_auth::API_TOKEN_ENV`] and never includes the
//! token value.
//!
//! GitHub access stays on `gh` in the parent module.

use super::Config;
use chat_client_contract::api_auth::API_TOKEN_ENV;
use reqwest::StatusCode;
use reqwest::blocking::Response;
use serde_json::{Value, json};
use std::time::Duration;

/// A daemon goal call failed. Display and Debug are the operator-facing line.
/// The process prints `Error: {err:?}` on stderr, so Debug must be that line.
pub(super) struct DaemonGoalError {
    message: String,
}

impl std::fmt::Display for DaemonGoalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::fmt::Debug for DaemonGoalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DaemonGoalError {}

pub(super) fn active_goals(cfg: &Config) -> Result<usize, DaemonGoalError> {
    goal_count(cfg, chat_client_contract::api_auth::bearer_token_from_env())
}

pub(super) fn goal_status(
    cfg: &Config,
    id: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match status_of(
        cfg,
        id,
        chat_client_contract::api_auth::bearer_token_from_env(),
    ) {
        Ok(status) => Ok(status),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn start_goal_with(
    cfg: &Config,
    description: String,
    max_turns: u32,
    id: Option<&str>,
    extra_payload: Value,
) -> Result<Option<String>, DaemonGoalError> {
    let body = goal_body(cfg, &description, max_turns, id, &extra_payload);
    post_goal(
        cfg,
        chat_client_contract::api_auth::bearer_token_from_env(),
        &body,
    )
}

pub(super) fn start_goal(
    cfg: &Config,
    description: String,
    max_turns: u32,
) -> Result<Option<String>, DaemonGoalError> {
    start_goal_with(cfg, description, max_turns, None, json!({}))
}

pub(super) fn launch_seed_task(
    cfg: &Config,
    dry: bool,
    task: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if dry {
        return Ok(());
    }
    start_seed_goal(cfg, task)
}

pub(super) fn require_goal_id(
    started: Result<Option<String>, DaemonGoalError>,
) -> Result<String, Box<dyn std::error::Error>> {
    match started {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err("repair task service did not accept the command".into()),
        Err(error) => Err(error.into()),
    }
}

/// One PR tick. A daemon goal failure stops the pass. Any other tick error is
/// logged and the pass continues, which is the previous behavior.
pub(super) fn finish_pr_tick(
    cfg: &Config,
    pr: &mut super::Pr,
    dry: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Err(error) = super::tick(cfg, pr, dry) {
        return propagate_tick_error(cfg, pr.number, error);
    }
    Ok(())
}

pub(super) fn slot_open(cfg: &Config, failure: &mut Option<DaemonGoalError>) -> bool {
    match open_slot(active_goals(cfg), cfg.max_concurrent) {
        Ok(free) => free,
        Err(error) => {
            *failure = Some(error);
            false
        }
    }
}

pub(super) fn open_slot(
    count: Result<usize, DaemonGoalError>,
    max_concurrent: usize,
) -> Result<bool, DaemonGoalError> {
    Ok(count? < max_concurrent)
}

pub(super) fn parse_goal_status(value: &Value) -> Option<String> {
    value.get("session").unwrap_or(value)["status"]
        .as_str()
        .map(|status| status.to_ascii_lowercase())
}

fn goal_count(cfg: &Config, token: Option<String>) -> Result<usize, DaemonGoalError> {
    let present = supplied_token(&token);
    let response = send_get(cfg, "/api/goals", token);
    match interpret_goal_list("/api/goals", present, response) {
        Ok(count) => Ok(count),
        Err(message) => Err(report(cfg, message)),
    }
}

fn status_of(
    cfg: &Config,
    id: &str,
    token: Option<String>,
) -> Result<Option<String>, DaemonGoalError> {
    let path = format!("/api/goals/{id}");
    let present = supplied_token(&token);
    let response = send_get(cfg, &path, token);
    match interpret_status(&path, present, response) {
        Ok(status) => Ok(status),
        Err(message) => Err(report(cfg, message)),
    }
}

fn post_goal(
    cfg: &Config,
    token: Option<String>,
    body: &Value,
) -> Result<Option<String>, DaemonGoalError> {
    let present = supplied_token(&token);
    let response = send_post(cfg, "/api/goals", token, body);
    match interpret_started("/api/goals", present, response) {
        Ok(id) => Ok(id),
        Err(message) => Err(report(cfg, message)),
    }
}

fn start_seed_goal(cfg: &Config, task: &str) -> Result<(), Box<dyn std::error::Error>> {
    wait_until_slot(cfg)?;
    let _started = start_goal(cfg, task.to_string(), 0)?;
    Ok(())
}

fn wait_until_slot(cfg: &Config) -> Result<(), DaemonGoalError> {
    while active_goals(cfg)? >= cfg.max_concurrent {
        std::thread::sleep(Duration::from_secs(cfg.poll));
    }
    Ok(())
}

fn goal_body(
    cfg: &Config,
    description: &str,
    max_turns: u32,
    id: Option<&str>,
    extra_payload: &Value,
) -> Value {
    let mut payload = json!({"project": cfg.project, "interactive": false});
    if let (Some(target), Some(extra)) = (payload.as_object_mut(), extra_payload.as_object()) {
        target.extend(extra.clone());
    }
    json!({
        "id": id,
        "description": description,
        "domain": "coding",
        "max_turns": max_turns,
        "profile": cfg.profile,
        "payload": payload,
    })
}

fn propagate_tick_error(
    cfg: &Config,
    pr_number: u64,
    error: Box<dyn std::error::Error>,
) -> Result<(), Box<dyn std::error::Error>> {
    if error.downcast_ref::<DaemonGoalError>().is_some() {
        return Err(error);
    }
    super::log(
        cfg,
        "tick_error",
        json!({"pr": pr_number, "detail": error.to_string()}),
    );
    Ok(())
}

fn report(cfg: &Config, message: String) -> DaemonGoalError {
    super::log(cfg, "daemon_goal_error", json!({"detail": message}));
    DaemonGoalError { message }
}

fn supplied_token(token: &Option<String>) -> bool {
    chat_client_contract::api_auth::authorization_value(token.as_deref()).is_some()
}

fn send_get(cfg: &Config, path: &str, token: Option<String>) -> Result<Response, reqwest::Error> {
    let http = http_client()?;
    let url = format!("{}{path}", cfg.daemon);
    super::daemon_http::daemon_get(&http, &url, || token).send()
}

fn send_post(
    cfg: &Config,
    path: &str,
    token: Option<String>,
    body: &Value,
) -> Result<Response, reqwest::Error> {
    let http = http_client()?;
    let url = format!("{}{path}", cfg.daemon);
    super::daemon_http::daemon_post(&http, &url, || token)
        .json(body)
        .send()
}

fn http_client() -> Result<reqwest::blocking::Client, reqwest::Error> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
}

fn interpret_goal_list(
    path: &str,
    token_present: bool,
    response: Result<Response, reqwest::Error>,
) -> Result<usize, String> {
    let response = received(path, response)?;
    counted_list(path, token_present, response)
}

fn interpret_status(
    path: &str,
    token_present: bool,
    response: Result<Response, reqwest::Error>,
) -> Result<Option<String>, String> {
    let response = received(path, response)?;
    status_body(path, token_present, response)
}

fn interpret_started(
    path: &str,
    token_present: bool,
    response: Result<Response, reqwest::Error>,
) -> Result<Option<String>, String> {
    let response = received(path, response)?;
    started_body(path, token_present, response)
}

fn received(path: &str, response: Result<Response, reqwest::Error>) -> Result<Response, String> {
    match response {
        Ok(response) => Ok(response),
        Err(error) => Err(transport_message(path, &error)),
    }
}

fn counted_list(path: &str, token_present: bool, response: Response) -> Result<usize, String> {
    let status = response.status();
    if let Some(message) = http_rejection(path, status, token_present) {
        return Err(message);
    }
    count_json_list(path, status, response)
}

fn count_json_list(path: &str, status: StatusCode, response: Response) -> Result<usize, String> {
    let body = json_body(path, status, response)?;
    listed_count(path, status, &body)
}

fn json_body(path: &str, status: StatusCode, response: Response) -> Result<Value, String> {
    match response.json::<Value>() {
        Ok(body) => Ok(body),
        Err(_) => Err(json_message(path, status)),
    }
}

fn listed_count(path: &str, status: StatusCode, body: &Value) -> Result<usize, String> {
    match count_active(body) {
        Some(count) => Ok(count),
        None => Err(not_a_list_message(path, status)),
    }
}

fn status_body(
    path: &str,
    token_present: bool,
    response: Response,
) -> Result<Option<String>, String> {
    let status = response.status();
    if status.as_u16() == 404 {
        return Ok(Some("missing".into()));
    }
    if let Some(message) = http_rejection(path, status, token_present) {
        return Err(message);
    }
    parse_status_json(path, status, response)
}

fn parse_status_json(
    path: &str,
    status: StatusCode,
    response: Response,
) -> Result<Option<String>, String> {
    match response.json::<Value>() {
        Ok(body) => Ok(parse_goal_status(&body)),
        Err(_) => Err(json_message(path, status)),
    }
}

fn started_body(
    path: &str,
    token_present: bool,
    response: Response,
) -> Result<Option<String>, String> {
    let status = response.status();
    if let Some(message) = http_rejection(path, status, token_present) {
        return Err(message);
    }
    match response.json::<Value>() {
        Ok(body) => Ok(goal_id(&body)),
        Err(_) => Err(json_message(path, status)),
    }
}

fn goal_id(value: &Value) -> Option<String> {
    if let Some(id) = value["id"].as_str() {
        return Some(id.to_owned());
    }
    value["session_id"].as_str().map(str::to_owned)
}

fn count_active(value: &Value) -> Option<usize> {
    let rows = value.as_array()?;
    let mut count = 0;
    for row in rows {
        if goal_is_active(row) {
            count += 1;
        }
    }
    Some(count)
}

fn goal_is_active(row: &Value) -> bool {
    match row["status"].as_str() {
        Some(status) => is_open_goal(status),
        None => false,
    }
}

fn is_open_goal(status: &str) -> bool {
    let status = status.to_ascii_lowercase();
    ["running", "pending", "starting", "active"].contains(&status.as_str())
}

fn http_rejection(path: &str, status: StatusCode, token_present: bool) -> Option<String> {
    if status.is_success() {
        None
    } else {
        Some(rejection_line(path, status, token_present))
    }
}

fn rejection_line(path: &str, status: StatusCode, token_present: bool) -> String {
    if is_denied(status) {
        denied_line(path, status, token_present)
    } else {
        format!(
            "liberado daemon rejected shepherd request to {path}: HTTP {} {}",
            status.as_u16(),
            phrase(status),
        )
    }
}

fn is_denied(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 403)
}

fn denied_line(path: &str, status: StatusCode, token_present: bool) -> String {
    format!(
        "liberado daemon rejected shepherd request to {path}: HTTP {} {} — set {API_TOKEN_ENV} ({})",
        status.as_u16(),
        phrase(status),
        token_hint(token_present),
    )
}

fn phrase(status: StatusCode) -> &'static str {
    match status.canonical_reason() {
        Some(text) => text,
        None => "error",
    }
}

fn token_hint(token_present: bool) -> &'static str {
    if token_present {
        "present but rejected"
    } else {
        "missing"
    }
}

fn transport_message(path: &str, error: &reqwest::Error) -> String {
    format!("liberado daemon request to {path} failed: {error}")
}

fn json_message(path: &str, status: StatusCode) -> String {
    format!(
        "liberado daemon request to {path} failed: HTTP {} response was not JSON",
        status.as_u16()
    )
}

fn not_a_list_message(path: &str, status: StatusCode) -> String {
    format!(
        "liberado daemon request to {path} failed: HTTP {} response was not a goal list",
        status.as_u16()
    )
}

#[cfg(test)]
#[path = "goal_client_tests.rs"]
mod tests;
