//! Token-scoped Done and Snooze for the service worker.
//!
//! The worker cannot send the owner cookie or the CSRF header. The per-row push
//! token is the capability, checked the same way as `POST /api/outbox/ack`.
//! It authorizes only that row, and only the complete and snooze transitions
//! the owner routes already perform for a completable vault task.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::{DateTime, Utc};
use liberado_notify::web_push::DEFAULT_SNOOZE_MINUTES;
use liberado_outbox::OutboxRecord;
use serde::Deserialize;

use crate::state::AppState;

use super::outbox_apply::{ActionFail, apply_complete, apply_snooze, fail_result, is_vault_task};
use super::outbox_detail::done_notice;
use super::outbox_log::{self, VIA_TRAY};

const ACTION_DONE: &str = "done";
const ACTION_SNOOZE: &str = "snooze";

#[derive(Deserialize)]
pub struct ActionBody {
    id: i64,
    token: String,
    action: String,
    #[serde(default)]
    minutes: Option<i64>,
}

#[derive(Clone, Copy)]
enum TrayAction {
    Done,
    Snooze,
}

pub async fn outbox_action_with_state(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ActionBody>,
) -> impl IntoResponse {
    match dispatch_push_action(state.as_ref(), body).await {
        Ok(notice) => (
            StatusCode::OK,
            Json(serde_json::json!({ "notice": notice })),
        )
            .into_response(),
        Err(err) => err.into_response(),
    }
}

async fn dispatch_push_action(state: &AppState, body: ActionBody) -> Result<String, ActionFail> {
    let received_at = Utc::now();
    let action = match parse_action(&body.action) {
        Ok(action) => action,
        Err(err) => {
            outbox_log::log_outbox_action("unknown", VIA_TRAY, body.id, received_at, "error");
            return Err(err);
        }
    };
    run_tray_action(state, body, action, received_at).await
}

async fn run_tray_action(
    state: &AppState,
    body: ActionBody,
    action: TrayAction,
    received_at: DateTime<Utc>,
) -> Result<String, ActionFail> {
    if let Err(err) = authorize_row(state, body.id, &body.token) {
        log_tray_stop(action, body.id, body.minutes, received_at, "error");
        return Err(err);
    }
    let record = match load_record(state, body.id) {
        Ok(record) => record,
        Err(err) => {
            log_tray_stop(
                action,
                body.id,
                body.minutes,
                received_at,
                fail_result(&err),
            );
            return Err(err);
        }
    };
    if let Err(err) = reject_if_resolved(&record) {
        log_tray_stop(
            action,
            body.id,
            body.minutes,
            received_at,
            "already_resolved",
        );
        return Err(err);
    }
    if let Err(err) = ensure_tray_task(&record) {
        log_tray_stop(action, body.id, body.minutes, received_at, "error");
        return Err(err);
    }
    match action {
        TrayAction::Done => finish_done(state, body.id, &record).await,
        TrayAction::Snooze => {
            let minutes = body.minutes.unwrap_or(DEFAULT_SNOOZE_MINUTES);
            apply_snooze(state, body.id, minutes, VIA_TRAY).await
        }
    }
}

fn log_tray_stop(
    action: TrayAction,
    id: i64,
    minutes: Option<i64>,
    received_at: DateTime<Utc>,
    result: &'static str,
) {
    match action {
        TrayAction::Snooze => {
            let minutes = minutes.unwrap_or(DEFAULT_SNOOZE_MINUTES);
            outbox_log::log_snooze_stopped(VIA_TRAY, id, minutes, received_at, result);
        }
        TrayAction::Done => {
            outbox_log::log_outbox_action("complete", VIA_TRAY, id, received_at, result);
        }
    }
}

async fn finish_done(
    state: &AppState,
    id: i64,
    record: &OutboxRecord,
) -> Result<String, ActionFail> {
    let notice = done_notice(state, record);
    apply_complete(state, id, VIA_TRAY).await?;
    Ok(notice)
}

fn parse_action(action: &str) -> Result<TrayAction, ActionFail> {
    match action {
        ACTION_DONE => Ok(TrayAction::Done),
        ACTION_SNOOZE => Ok(TrayAction::Snooze),
        _ => Err(ActionFail::bad("unsupported action")),
    }
}

fn authorize_row(state: &AppState, id: i64, token: &str) -> Result<(), ActionFail> {
    let Some(outbox) = state.outbox.as_deref() else {
        return Err(ActionFail::Unavailable);
    };
    match outbox.push_token_matches(id, token) {
        Ok(true) => Ok(()),
        Ok(false) => Err(ActionFail::Forbidden),
        Err(err) => Err(ActionFail::internal(err.to_string())),
    }
}

fn load_record(state: &AppState, id: i64) -> Result<OutboxRecord, ActionFail> {
    let Some(outbox) = state.outbox.as_deref() else {
        return Err(ActionFail::Unavailable);
    };
    match outbox.fetch_record(id) {
        Ok(Some(record)) => Ok(record),
        Ok(None) => Err(ActionFail::NotFound),
        Err(err) => Err(ActionFail::internal(err.to_string())),
    }
}

fn reject_if_resolved(record: &OutboxRecord) -> Result<(), ActionFail> {
    if record.resolved_at.is_none() {
        return Ok(());
    }
    let existing = record.resolution.as_deref().unwrap_or("resolved");
    Err(ActionFail::conflict(format!(
        "row already resolved as {existing}"
    )))
}

fn ensure_tray_task(record: &OutboxRecord) -> Result<(), ActionFail> {
    if is_vault_task(record) {
        return Ok(());
    }
    Err(ActionFail::bad(
        "notification does not support tray actions",
    ))
}
