//! Weekly spending trigger.
//!
//! Subscription admin lives in `reminders`. This route only builds the Monday
//! message and fans it out through the outbox subscriptions.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use liberado_daemon::{
    HttpBudget, RunMode, RunOutcome, SpendingSource, WeekPush, WeekSender, inert_week_sender,
    local_now, run_weekly,
};
use serde::Deserialize;

use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct WeeklyBody {
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    force: bool,
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
    let sender = match week_sender(&state, mode) {
        Ok(sender) => sender,
        Err(err) => return fail(StatusCode::SERVICE_UNAVAILABLE, &err),
    };
    match run_one(&state, source, sender, mode).await {
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

fn week_sender(state: &AppState, mode: RunMode) -> Result<Arc<dyn WeekSender>, String> {
    if matches!(mode, RunMode::DryRun) {
        return Ok(inert_week_sender());
    }
    let outbox = state
        .outbox
        .clone()
        .ok_or_else(|| "outbox is not available".to_string())?;
    let keys = state
        .outbox_delivery
        .as_ref()
        .and_then(|delivery| delivery.vapid_keys());
    Ok(Arc::new(WeekPush::new(outbox, keys)))
}

async fn run_one(
    state: &AppState,
    source: Arc<dyn SpendingSource>,
    sender: Arc<dyn WeekSender>,
    mode: RunMode,
) -> Result<RunOutcome, String> {
    let cfg = &state.config.topology.weekly_spending;
    let now = local_now(None, &cfg.timezone);
    let (hour, minute) = liberado_config::weekly_open_clock(&state.config.topology);
    run_weekly(cfg, now, hour, minute, source, sender, mode).await
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
