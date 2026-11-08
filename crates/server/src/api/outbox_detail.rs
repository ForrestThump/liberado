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
use liberado_outbox::{OpenRow, OutboxRecord, OutboxSource, RES_SNOOZED};
use serde::Deserialize;
use serde::Serialize;

use crate::state::AppState;

use super::outbox_apply::{apply_complete, apply_snooze, fail};
use super::outbox_log::VIA_DETAIL;

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

pub(crate) fn done_notice(state: &AppState, record: &OutboxRecord) -> String {
    let zone = operator_zone(state);
    let open = open_row_from_record(record);
    let (title, _, _) = liberado_outbox::copy::present_row(&open, Utc::now(), &zone);
    format!("Done: {title}")
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

pub(crate) fn snooze_until_local(
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
