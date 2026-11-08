//! Authenticated read and owner actions for a single outbox reminder row.

use std::path::Path;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use liberado_outbox::{
    OpenRow, OutboxRecord, OutboxSource, OwnerResolveResult, OwnerSnoozeResult, RES_COMPLETED,
    RES_DISMISSED, RES_SNOOZED, TASK_DUE,
};
use liberado_vault::Vault;
use serde::Deserialize;
use serde::Serialize;

use crate::state::AppState;

use super::outbox_log::{self, SnoozeApplied, VIA_DETAIL};
use super::vault_task_line::toggle_open_task_line;

#[derive(Serialize)]
pub struct OutboxDetailResponse {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub kind: String,
    pub due_at: String,
    pub due_at_local: String,
    pub late: bool,
    pub catch_up: bool,
    pub source: Option<OutboxSource>,
    pub state: String,
    pub delivered: bool,
    pub acked: bool,
    pub resolution: Option<String>,
    pub resolved_at: Option<String>,
    pub snooze_until_local: Option<String>,
    pub obsidian_url: Option<String>,
}

#[derive(Deserialize)]
pub struct SnoozeBody {
    pub minutes: i64,
}

pub async fn get_outbox_detail(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<i64>,
) -> Response {
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    let Some(record) = (match outbox.fetch_record(id) {
        Ok(row) => row,
        Err(err) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }) else {
        return fail(StatusCode::NOT_FOUND, "outbox row not found");
    };
    let zone = operator_zone(&state);
    let now = Utc::now();
    let open = open_row_from_record(&record);
    let (title, body, late) = liberado_outbox::copy::present_row(&open, now, &zone);
    Json(detail_json(
        &record,
        title,
        body,
        late,
        &zone,
        obsidian_url_for(&state.config.topology, record.source.as_ref()),
        snooze_until_local(&state, outbox.as_ref(), &record),
    ))
    .into_response()
}

pub async fn complete_outbox(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<i64>,
) -> Response {
    match apply_complete(state.as_ref(), id, VIA_DETAIL).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn snooze_outbox(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<SnoozeBody>,
) -> Response {
    match apply_snooze(state.as_ref(), id, body.minutes, VIA_DETAIL).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => err.into_response(),
    }
}

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

/// Same complete transition the owner route uses. Vault tasks are checked off
/// before the row is claimed, so a failed note write leaves the row open.
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
    if vault_task && let Err(err) = write_vault_task(state, &record).await {
        outbox_log::log_outbox_action(action, via, record.id, received_at, "error");
        return Err(err);
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

fn fail_result(err: &ActionFail) -> &'static str {
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

pub(crate) fn done_notice(state: &AppState, record: &OutboxRecord) -> String {
    let zone = operator_zone(state);
    let open = open_row_from_record(record);
    let (title, _, _) = liberado_outbox::copy::present_row(&open, Utc::now(), &zone);
    format!("Done: {title}")
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

async fn write_vault_task(state: &AppState, record: &OutboxRecord) -> Result<(), ActionFail> {
    match &record.source {
        Some(OutboxSource::VaultNote { path, text, .. }) => {
            complete_vault_task(&state.vault_path, path, text)
                .await
                .map_err(ActionFail::conflict)
        }
        _ => Err(ActionFail::internal("vault task missing source")),
    }
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

pub fn obsidian_vault_display_name(vault_path: &Path, configured: Option<&str>) -> String {
    if let Some(name) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        return name.to_string();
    }
    vault_path
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or("vault")
        .to_string()
}

pub fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn obsidian_open_url(vault_name: &str, rel_path: &str) -> String {
    format!(
        "obsidian://open?vault={}&file={}",
        encode_uri_component(vault_name),
        encode_uri_component(rel_path)
    )
}

pub fn obsidian_url_for(
    topology: &liberado_config::Topology,
    source: Option<&OutboxSource>,
) -> Option<String> {
    let OutboxSource::VaultNote { path, .. } = source? else {
        return None;
    };
    let name = obsidian_vault_display_name(
        &topology.vault_path,
        topology.obsidian_vault_name.as_deref(),
    );
    Some(obsidian_open_url(&name, path))
}

fn snooze_until_local(
    state: &AppState,
    outbox: &liberado_outbox::Outbox,
    record: &OutboxRecord,
) -> Option<String> {
    if record.resolution.as_deref() != Some(RES_SNOOZED) {
        return None;
    }
    let key = format!("snooze/{}", record.id);
    let due = outbox.due_at_by_idempotency(&key).ok()??;
    let zone = operator_zone(state);
    Some(liberado_outbox::copy::format_time_12h_for_zone(&zone, due))
}

fn detail_json(
    record: &OutboxRecord,
    title: String,
    body: String,
    late: bool,
    zone_name: &str,
    obsidian_url: Option<String>,
    snooze_until_local: Option<String>,
) -> OutboxDetailResponse {
    OutboxDetailResponse {
        id: record.id,
        title,
        body,
        kind: record.kind.clone(),
        due_at: record.due_at.to_rfc3339(),
        due_at_local: liberado_outbox::copy::format_time_12h_for_zone(zone_name, record.due_at),
        late,
        catch_up: record.catch_up,
        source: record.source.clone(),
        state: record.state.clone(),
        delivered: record.state == "delivered",
        acked: record.acked,
        resolution: record.resolution.clone(),
        resolved_at: record.resolved_at.map(|at| at.to_rfc3339()),
        snooze_until_local,
        obsidian_url,
    }
}

fn operator_zone(state: &AppState) -> String {
    state
        .config
        .topology
        .user_timezone()
        .ok()
        .map(|tz| tz.iana_name().to_string())
        .unwrap_or_else(|| {
            liberado_common::UserTimezone::default_zone()
                .iana_name()
                .to_string()
        })
}

fn open_row_from_record(record: &OutboxRecord) -> OpenRow {
    OpenRow {
        id: record.id,
        channel: record.channel.clone(),
        target: record.target.clone(),
        title: record.title.clone(),
        body: record.body.clone(),
        catch_up: record.catch_up,
        due_at: record.due_at,
        token: String::new(),
        kind: record.kind.clone(),
        source: record.source.clone(),
    }
}

async fn complete_vault_task(
    vault_path: &str,
    rel_path: &str,
    task_text: &str,
) -> Result<(), String> {
    let vault = Vault::open("outbox-complete", vault_path)
        .await
        .map_err(|err| err.to_string())?;
    let content = vault.read(rel_path).await.map_err(|err| err.to_string())?;
    let updated = toggle_open_task_line(&content, task_text)?;
    let hash = Vault::content_hash(&content);
    vault
        .write(
            rel_path,
            &updated,
            Some(&hash),
            &liberado_common::WriteProvenance::human(),
        )
        .await
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obsidian_url_uses_uri_component_encoding() {
        let url = obsidian_open_url("My Vault", "Tasks/Inbox Note é.md");
        assert_eq!(
            url,
            "obsidian://open?vault=My%20Vault&file=Tasks%2FInbox%20Note%20%C3%A9.md"
        );
    }

    #[test]
    fn obsidian_vault_name_defaults_to_directory_basename() {
        assert_eq!(
            obsidian_vault_display_name(Path::new("/vault/Notes"), None),
            "Notes"
        );
    }
}
