//! Web Push subscription and the weekly spending trigger.
//!
//! Tailscale is the trust boundary. These routes are not agent tools.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use liberado_daemon::{HttpBudget, RunMode, RunOutcome, SpendingSource, local_now, run_weekly};
use liberado_notify::NewSubscription;
use serde::Deserialize;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct SubscribeBody {
    endpoint: String,
    keys: SubscribeKeys,
    #[serde(default)]
    label: String,
}

#[derive(Deserialize)]
struct SubscribeKeys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
pub struct EndpointBody {
    endpoint: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct TestBody {
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct WeeklyBody {
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    force: bool,
}

pub async fn push_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "vapid_configured": state.push.configured(),
        "reminder_enabled": state.config.topology.weekly_spending.enabled,
        "subscription_count": state.push.subscription_count(),
        "public_key": state.push.vapid_public_key(),
    }))
}

pub async fn vapid_public_key(State(state): State<Arc<AppState>>) -> Response {
    match state.push.vapid_public_key() {
        Some(public_key) => Json(serde_json::json!({ "public_key": public_key })).into_response(),
        None => fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Web Push keys are not configured",
        ),
    }
}

pub async fn upsert_subscription(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SubscribeBody>,
) -> Response {
    let saved = state.push.upsert(NewSubscription {
        endpoint: body.endpoint,
        p256dh: body.keys.p256dh,
        auth: body.keys.auth,
        label: body.label,
    });
    match saved {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(err) => fail(StatusCode::BAD_REQUEST, &err),
    }
}

pub async fn delete_subscription(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EndpointBody>,
) -> Response {
    match state.push.remove_endpoint(&body.endpoint) {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(err) => fail(StatusCode::BAD_REQUEST, &err),
    }
}

pub async fn push_test(State(state): State<Arc<AppState>>, Json(body): Json<TestBody>) -> Response {
    if let Some(response) = refuse_test(&state, body.endpoint.as_deref()) {
        return response;
    }
    let report = state
        .push
        .send_text(
            body.endpoint.as_deref(),
            "Liberado test",
            "Liberado test notification.",
        )
        .await;
    test_report(report)
}

pub async fn push_weekly(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WeeklyBody>,
) -> Response {
    let mode = weekly_mode(&body);
    if let Some(response) = weekly_blocked(state.config.topology.weekly_spending.enabled, mode) {
        return response;
    }
    let source = match budget_source(&state) {
        Ok(source) => source,
        Err(err) => return fail(budget_status(&err), &err),
    };
    match run_one(&state, source, mode).await {
        Ok(outcome) => Json(outcome_json(&outcome)).into_response(),
        Err(err) => fail(budget_status(&err), &err),
    }
}

fn weekly_mode(body: &WeeklyBody) -> RunMode {
    if body.dry_run {
        return RunMode::DryRun;
    }
    if body.force {
        return RunMode::Force;
    }
    RunMode::Send
}

fn weekly_blocked(enabled: bool, mode: RunMode) -> Option<Response> {
    if enabled || matches!(mode, RunMode::DryRun) {
        return None;
    }
    Some(fail(StatusCode::CONFLICT, "weekly spending is off"))
}

fn budget_source(state: &AppState) -> Result<Arc<dyn SpendingSource>, String> {
    if let Some(source) = state.spending.clone() {
        return Ok(source);
    }
    HttpBudget::from_env().map(|client| Arc::new(client) as Arc<dyn SpendingSource>)
}

async fn run_one(
    state: &AppState,
    source: Arc<dyn SpendingSource>,
    mode: RunMode,
) -> Result<RunOutcome, String> {
    let cfg = &state.config.topology.weekly_spending;
    let now = local_now(None, &cfg.timezone);
    let (hour, minute) = liberado_config::weekly_open_clock(&state.config.topology);
    run_weekly(
        cfg,
        now,
        hour,
        minute,
        source,
        Arc::clone(&state.push),
        mode,
    )
    .await
}

fn refuse_test(state: &AppState, endpoint: Option<&str>) -> Option<Response> {
    if !state.push.configured() {
        return Some(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Web Push keys are not configured",
        ));
    }
    if endpoint.is_none() && state.push.subscription_count() == 0 {
        return Some(fail(StatusCode::CONFLICT, "no web push subscriptions"));
    }
    None
}

fn test_report(report: liberado_notify::SendReport) -> Response {
    if report.missing {
        return fail(StatusCode::NOT_FOUND, "subscription not found");
    }
    if report.unconfigured {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Web Push keys are not configured",
        );
    }
    Json(serde_json::json!({
        "sent": report.sent,
        "failed": report.failed,
        "removed": report.removed,
    }))
    .into_response()
}

fn outcome_json(outcome: &RunOutcome) -> serde_json::Value {
    serde_json::json!({
        "message": outcome.message,
        "week_id": outcome.week_id,
        "start": outcome.start,
        "end": outcome.end,
        "unmapped": outcome.unmapped,
        "sent": outcome.sent,
        "failed": outcome.failed,
        "skipped": outcome.skipped,
        "removed": outcome.removed,
    })
}

fn budget_status(err: &str) -> StatusCode {
    if err == "budget API is not configured" || err == "weekly spending is off" {
        return status_for_known(err);
    }
    StatusCode::BAD_GATEWAY
}

fn status_for_known(err: &str) -> StatusCode {
    if err == "weekly spending is off" {
        return StatusCode::CONFLICT;
    }
    StatusCode::SERVICE_UNAVAILABLE
}

fn fail(status: StatusCode, error: &str) -> Response {
    (status, Json(serde_json::json!({ "error": error }))).into_response()
}

#[cfg(test)]
#[path = "push_http_tests.rs"]
mod push_http_tests;
