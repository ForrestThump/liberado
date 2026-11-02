//! Authenticated read and owner actions for a single outbox reminder row.

use std::path::Path;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use liberado_common::WriteProvenance;
use liberado_outbox::{NewReminder, OpenRow, OutboxRecord, OutboxSource, TASK_DUE, copy};
use liberado_vault::Vault;
use serde::Deserialize;
use serde::Serialize;

use crate::state::AppState;

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
    let (title, body, late) = copy::present_row(&open, now, &zone);
    Json(detail_json(
        &record,
        title,
        body,
        late,
        &zone,
        obsidian_url_for(&state.config.topology, record.source.as_ref()),
    ))
    .into_response()
}

pub async fn complete_outbox(
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
    if record.kind == TASK_DUE
        && let Some(OutboxSource::VaultNote { path, text, .. }) = &record.source
        && let Err(message) = complete_vault_task(&state.vault_path, path, text).await
    {
        return fail(StatusCode::CONFLICT, &message);
    }
    let now = Utc::now();
    match outbox.owner_ack(id, now) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => fail(StatusCode::CONFLICT, "row already acked or not ackable"),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub async fn snooze_outbox(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<SnoozeBody>,
) -> Response {
    if body.minutes <= 0 || body.minutes > 60 * 24 * 7 {
        return fail(StatusCode::BAD_REQUEST, "minutes must be between 1 and 10080");
    }
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    let Some(record) = (match outbox.fetch_record(id) {
        Ok(row) => row,
        Err(err) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }) else {
        return fail(StatusCode::NOT_FOUND, "outbox row not found");
    };
    let now = Utc::now();
    let due = now + Duration::minutes(body.minutes);
    let title = format!("Snoozed: {}", record.title);
    let key = format!("snooze/{}/{}", id, now.timestamp());
    let reminder = NewReminder {
        idempotency_key: key,
        channel: record.channel,
        target: record.target,
        title,
        body: record.body,
        due_at: due,
        catch_up: false,
        kind: Some(record.kind),
        source: record.source,
    };
    if let Err(err) = outbox.enqueue(&reminder, now) {
        return fail(StatusCode::BAD_REQUEST, &err.to_string());
    }
    match outbox.owner_ack(id, now) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => fail(StatusCode::CONFLICT, "row already acked or not ackable"),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub fn obsidian_vault_display_name(
    vault_path: &Path,
    configured: Option<&str>,
) -> String {
    if let Some(name) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        return name.to_string();
    }
    vault_path
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or("vault")
        .to_string()
}

pub fn obsidian_open_url(vault_name: &str, rel_path: &str) -> String {
    let mut url = url::Url::parse("obsidian://open").expect("obsidian scheme");
    url.query_pairs_mut()
        .append_pair("vault", vault_name)
        .append_pair("file", rel_path);
    url.to_string()
}

pub fn obsidian_url_for(topology: &liberado_config::Topology, source: Option<&OutboxSource>) -> Option<String> {
    let OutboxSource::VaultNote { path, .. } = source? else {
        return None;
    };
    let name = obsidian_vault_display_name(
        &topology.vault_path,
        topology.obsidian_vault_name.as_deref(),
    );
    Some(obsidian_open_url(&name, path))
}

fn detail_json(
    record: &OutboxRecord,
    title: String,
    body: String,
    late: bool,
    zone_name: &str,
    obsidian_url: Option<String>,
) -> OutboxDetailResponse {
    OutboxDetailResponse {
        id: record.id,
        title,
        body,
        kind: record.kind.clone(),
        due_at: record.due_at.to_rfc3339(),
        due_at_local: copy::format_time_12h_for_zone(zone_name, record.due_at),
        late,
        catch_up: record.catch_up,
        source: record.source.clone(),
        state: record.state.clone(),
        delivered: record.state == "delivered",
        acked: record.acked,
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
        .unwrap_or_else(|| liberado_common::UserTimezone::default_zone().iana_name().to_string())
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

async fn complete_vault_task(vault_path: &str, rel_path: &str, task_text: &str) -> Result<(), String> {
    let vault = Vault::open("outbox-complete", vault_path)
        .await
        .map_err(|err| err.to_string())?;
    let content = vault
        .read(rel_path)
        .await
        .map_err(|err| err.to_string())?;
    let updated = toggle_open_task_line(&content, task_text)?;
    let hash = Vault::content_hash(&content);
    vault
        .write(
            rel_path,
            &updated,
            Some(&hash),
            &WriteProvenance::human(),
        )
        .await
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn toggle_open_task_line(content: &str, task_text: &str) -> Result<String, String> {
    let needle = task_text.trim();
    if needle.is_empty() {
        return Err("task text is empty".into());
    }
    let mut hits = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if line.contains("- [ ]") && line_contains_task(line, needle) {
            hits.push(index);
        }
    }
    if hits.is_empty() {
        return Err("no matching open task line".into());
    }
    if hits.len() > 1 {
        return Err("task line is ambiguous".into());
    }
    let idx = hits[0];
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    lines[idx] = lines[idx].replacen("- [ ]", "- [x]", 1);
    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn line_contains_task(line: &str, task_text: &str) -> bool {
    let stripped = line
        .split("- [ ]")
        .nth(1)
        .unwrap_or(line)
        .trim();
    stripped.contains(task_text) || task_text.contains(stripped)
}

fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn obsidian_url_encodes_spaces_and_slashes() {
        let topology = liberado_config::Topology {
            vault_path: PathBuf::from("/data/My Vault"),
            obsidian_vault_name: Some("Personal".into()),
            ..liberado_config::Topology::default()
        };
        let url = obsidian_url_for(
            &topology,
            Some(&OutboxSource::VaultNote {
                path: "Tasks/Inbox Note.md".into(),
                label: "due".into(),
                text: "Call dentist".into(),
            }),
        )
        .expect("url");
        assert!(url.starts_with("obsidian://open?"));
        assert!(url.contains("vault=Personal"));
        assert!(url.contains("Tasks%2FInbox%20Note.md") || url.contains("Inbox+Note"));
    }

    #[test]
    fn obsidian_vault_name_defaults_to_directory_basename() {
        assert_eq!(
            obsidian_vault_display_name(Path::new("/vault/Notes"), None),
            "Notes"
        );
    }

    #[test]
    fn toggle_task_line_refuses_ambiguous_matches() {
        let note = "- [ ] one\n- [ ] one\n";
        assert!(toggle_open_task_line(note, "one").is_err());
    }

    #[test]
    fn toggle_task_line_updates_single_match() {
        let note = "- [ ] Call dentist\n";
        let out = toggle_open_task_line(note, "Call dentist").unwrap();
        assert!(out.contains("- [x] Call dentist"));
    }
}
