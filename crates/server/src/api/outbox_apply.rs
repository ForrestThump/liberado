//! Complete and snooze for one outbox row.
//!
//! The owner detail routes and the tray action route both apply these
//! transitions. Vault tasks are checked off before the row is claimed, so a
//! failed note write leaves the row open. The result log carries ids and
//! timestamps only.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use liberado_outbox::{
    OutboxRecord, OutboxSource, OwnerResolveResult, OwnerSnoozeResult, RES_COMPLETED,
    RES_DISMISSED, TASK_DUE,
};
use liberado_vault::Vault;

use crate::state::AppState;

use super::outbox_detail::snooze_until_local;
use super::outbox_log::{self, SnoozeApplied};
use super::vault_task_line::{TaskLineOutcome, plan_task_edit};

const MAX_SNOOZE_MINUTES: i64 = 60 * 24 * 7;

pub(crate) enum ActionFail {
    Unavailable,
    NotFound,
    Forbidden,
    Conflict(String),
    BadRequest(String),
    Internal(String),
}

impl ActionFail {
    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    pub(crate) fn bad(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    pub(crate) fn into_response(self) -> Response {
        match self {
            Self::Unavailable => fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available"),
            Self::NotFound => fail(StatusCode::NOT_FOUND, "outbox row not found"),
            Self::Forbidden => fail(StatusCode::FORBIDDEN, "unknown or mismatched token"),
            Self::Conflict(message) => fail(StatusCode::CONFLICT, &message),
            Self::BadRequest(message) => fail(StatusCode::BAD_REQUEST, &message),
            Self::Internal(message) => fail(StatusCode::INTERNAL_SERVER_ERROR, &message),
        }
    }
}

pub(crate) fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// Same complete transition the owner route uses.
pub(crate) async fn apply_complete(
    state: &AppState,
    id: i64,
    via: &'static str,
) -> Result<(), ActionFail> {
    let received_at = Utc::now();
    let outbox = match require_outbox(state) {
        Ok(outbox) => outbox,
        Err(err) => {
            outbox_log::log_outbox_action("complete", via, id, received_at, "error");
            return Err(err);
        }
    };
    let record = match require_record(outbox, id) {
        Ok(record) => record,
        Err(err) => {
            outbox_log::log_outbox_action("complete", via, id, received_at, fail_result(&err));
            return Err(err);
        }
    };
    finish_complete(state, outbox, record, via, received_at).await
}

async fn finish_complete(
    state: &AppState,
    outbox: &liberado_outbox::Outbox,
    record: OutboxRecord,
    via: &str,
    received_at: chrono::DateTime<Utc>,
) -> Result<(), ActionFail> {
    let vault_task = is_vault_task(&record);
    let action = if vault_task { "complete" } else { "dismiss" };
    if vault_task {
        match write_vault_task(state, &record).await {
            Ok(VaultWrite::Checked) => {}
            Ok(VaultWrite::Recurring(identity)) => {
                if let Err(err) = record_recurring_tap(state, outbox, &record, &identity) {
                    outbox_log::log_outbox_action(action, via, record.id, received_at, "error");
                    return Err(err);
                }
            }
            Err(err) => {
                outbox_log::log_outbox_action(action, via, record.id, received_at, "error");
                return Err(err);
            }
        }
    }
    let resolution = if vault_task {
        RES_COMPLETED
    } else {
        RES_DISMISSED
    };
    let result = resolve_claimed(outbox, record.id, resolution);
    outbox_log::log_outbox_action(action, via, record.id, received_at, claimed_result(&result));
    result
}

/// Same snooze transition the owner route uses. The string is the tray confirmation.
pub(crate) async fn apply_snooze(
    state: &AppState,
    id: i64,
    minutes: i64,
    via: &'static str,
) -> Result<String, ActionFail> {
    if minutes <= 0 || minutes > MAX_SNOOZE_MINUTES {
        outbox_log::log_snooze_stopped(via, id, minutes, Utc::now(), "error");
        return Err(ActionFail::bad("minutes must be between 1 and 10080"));
    }
    let outbox = match require_outbox(state) {
        Ok(outbox) => outbox,
        Err(err) => {
            outbox_log::log_snooze_stopped(via, id, minutes, Utc::now(), "error");
            return Err(err);
        }
    };
    let now = Utc::now();
    let prior = record_due(outbox, id);
    match outbox.owner_snooze(id, minutes, now) {
        Ok(OwnerSnoozeResult::Applied { follow_up_id }) => {
            note_applied_snooze(via, id, minutes, now, prior, outbox, follow_up_id);
            Ok(snooze_notice(state, outbox, id))
        }
        Ok(OwnerSnoozeResult::AlreadyResolved(existing)) => {
            outbox_log::log_snooze_stopped(via, id, minutes, now, "already_resolved");
            Err(ActionFail::conflict(format!(
                "row already resolved as {existing}"
            )))
        }
        Ok(OwnerSnoozeResult::NotFound) => {
            outbox_log::log_snooze_stopped(via, id, minutes, now, "not_found");
            Err(ActionFail::NotFound)
        }
        Err(err) => {
            outbox_log::log_snooze_stopped(via, id, minutes, now, "error");
            Err(ActionFail::internal(err.to_string()))
        }
    }
}

fn note_applied_snooze(
    via: &str,
    id: i64,
    minutes: i64,
    now: chrono::DateTime<Utc>,
    prior: Option<chrono::DateTime<Utc>>,
    outbox: &liberado_outbox::Outbox,
    follow_up_id: i64,
) {
    let Some(prior_due_at) = prior else {
        outbox_log::log_outbox_action("snooze", via, id, now, "applied");
        return;
    };
    let Some(new_due_at) = follow_up_due(outbox, id) else {
        outbox_log::log_outbox_action("snooze", via, id, now, "applied");
        return;
    };
    outbox_log::log_snooze_applied(&SnoozeApplied {
        via,
        row_id: id,
        minutes,
        received_at: now,
        prior_due_at,
        new_due_at,
        follow_up_row_id: follow_up_id,
    });
}

fn record_due(outbox: &liberado_outbox::Outbox, id: i64) -> Option<chrono::DateTime<Utc>> {
    outbox
        .fetch_record(id)
        .ok()
        .flatten()
        .map(|record| record.due_at)
}

fn follow_up_due(outbox: &liberado_outbox::Outbox, id: i64) -> Option<chrono::DateTime<Utc>> {
    let key = format!("snooze/{id}");
    outbox.due_at_by_idempotency(&key).ok().flatten()
}

pub(crate) fn fail_result(err: &ActionFail) -> &'static str {
    match err {
        ActionFail::NotFound => "not_found",
        _ => "error",
    }
}

fn claimed_result(result: &Result<(), ActionFail>) -> &'static str {
    match result {
        Ok(()) => "applied",
        Err(ActionFail::NotFound) => "not_found",
        Err(ActionFail::Conflict(_)) => "already_resolved",
        Err(_) => "error",
    }
}

pub(crate) fn is_vault_task(record: &OutboxRecord) -> bool {
    record.kind == TASK_DUE && matches!(record.source, Some(OutboxSource::VaultNote { .. }))
}

fn require_outbox(state: &AppState) -> Result<&liberado_outbox::Outbox, ActionFail> {
    state.outbox.as_deref().ok_or(ActionFail::Unavailable)
}

fn require_record(outbox: &liberado_outbox::Outbox, id: i64) -> Result<OutboxRecord, ActionFail> {
    match outbox.fetch_record(id) {
        Ok(Some(record)) => Ok(record),
        Ok(None) => Err(ActionFail::NotFound),
        Err(err) => Err(ActionFail::internal(err.to_string())),
    }
}

fn resolve_claimed(
    outbox: &liberado_outbox::Outbox,
    id: i64,
    resolution: &str,
) -> Result<(), ActionFail> {
    match outbox.try_resolve(id, resolution, Utc::now()) {
        Ok(OwnerResolveResult::Applied) => Ok(()),
        Ok(OwnerResolveResult::AlreadyResolved(existing)) => Err(ActionFail::conflict(format!(
            "row already resolved as {existing}"
        ))),
        Ok(OwnerResolveResult::NotFound) => Err(ActionFail::NotFound),
        Err(err) => Err(ActionFail::internal(err.to_string())),
    }
}

async fn write_vault_task(
    state: &AppState,
    record: &OutboxRecord,
) -> Result<VaultWrite, ActionFail> {
    match &record.source {
        Some(OutboxSource::VaultNote { path, text, .. }) => {
            complete_vault_task(&state.vault_path, path, text).await
        }
        _ => Err(ActionFail::internal("vault task missing source")),
    }
}

enum VaultWrite {
    Checked,
    Recurring(String),
}

fn record_recurring_tap(
    state: &AppState,
    outbox: &liberado_outbox::Outbox,
    record: &OutboxRecord,
    identity: &str,
) -> Result<(), ActionFail> {
    let Some(OutboxSource::VaultNote { path, .. }) = &record.source else {
        return Err(ActionFail::internal("vault task missing source"));
    };
    let now = Utc::now();
    let zone = super::outbox_detail::operator_zone(state);
    let row = liberado_outbox::liberado_tap_completion(path, identity, record.id, now, &zone);
    outbox
        .insert_completion(&row)
        .map_err(|err| ActionFail::internal(err.to_string()))?;
    Ok(())
}

fn snooze_notice(state: &AppState, outbox: &liberado_outbox::Outbox, id: i64) -> String {
    let until = outbox
        .fetch_record(id)
        .ok()
        .flatten()
        .and_then(|record| snooze_until_local(state, outbox, &record));
    match until {
        Some(text) => format!("Snoozed until {text}"),
        None => "Snoozed".to_string(),
    }
}

async fn complete_vault_task(
    vault_path: &str,
    rel_path: &str,
    task_text: &str,
) -> Result<VaultWrite, ActionFail> {
    let vault = Vault::open("outbox-complete", vault_path)
        .await
        .map_err(|err| ActionFail::conflict(err.to_string()))?;
    let content = vault
        .read(rel_path)
        .await
        .map_err(|err| ActionFail::conflict(err.to_string()))?;
    match plan_task_edit(&content, task_text).map_err(ActionFail::conflict)? {
        TaskLineOutcome::Recurring { identity } => Ok(VaultWrite::Recurring(identity)),
        TaskLineOutcome::Write(updated) => {
            let hash = Vault::content_hash(&content);
            vault
                .write(
                    rel_path,
                    &updated,
                    Some(&hash),
                    &liberado_common::WriteProvenance::human(),
                )
                .await
                .map_err(|err| ActionFail::conflict(err.to_string()))?;
            Ok(VaultWrite::Checked)
        }
    }
}
