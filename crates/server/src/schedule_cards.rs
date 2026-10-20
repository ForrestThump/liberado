//! Schedule-edit cards and the one writer call behind Approve.
//!
//! The face tools file the note. This module applies it with the slice B writer
//! and tells the chat what happened. It does not reload cron or `AppState`.

use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chat_client_contract::{ApprovalOption, ApprovalResolveResponse};
use liberado_common::{DecisionVia, Proposal, ProposedAction, ToolCall};
use liberado_config::{
    CronSchedule, SchedulePatch, ScheduleWriteError, ScheduleWriteReport, create_schedule,
    delete_schedule, schedule_topology_path, update_schedule,
};
use liberado_conversation_store::Ulid;
use liberado_main_agent::{
    CREATE_SCHEDULE_TOOL_NAME, DELETE_SCHEDULE_TOOL_NAME, UPDATE_SCHEDULE_TOOL_NAME,
    is_schedule_write_tool,
};
use liberado_messaging::permission_card_options;
use liberado_telegram_approvals::ResolveOutcome;
use serde::Deserialize;

use crate::state::AppState;

/// `list_schedules` reads the file on disk, not the boot snapshot.
///
/// Each row includes `cron_expr`, code-generated `schedule_text`, and `next_fires`.
pub fn list_from_disk() -> Result<String, String> {
    let path = schedule_topology_path().map_err(|err| err.to_string())?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    let file: ScheduleFile = toml::from_str(&text)
        .map_err(|err| format!("could not parse {}: {err}", path.display()))?;
    schedules_json_with_local_clock(&file)
}

/// JSON the editor model reads. Timing fields are computed here, not written by the model.
fn schedules_json_with_local_clock(file: &ScheduleFile) -> Result<String, String> {
    let zone = file.timezone.trim();
    let mut rows = Vec::with_capacity(file.schedules.len());
    for schedule in &file.schedules {
        rows.push(schedule_row_with_local_clock(schedule, zone)?);
    }
    serde_json::to_string(&rows).map_err(|err| err.to_string())
}

fn schedule_row_with_local_clock(
    schedule: &CronSchedule,
    zone: &str,
) -> Result<serde_json::Value, String> {
    let mut value = serde_json::to_value(schedule).map_err(|err| err.to_string())?;
    let shown = crate::schedule_timing::show(schedule, zone).ok();
    let zone_name = shown
        .as_ref()
        .map(|row| row.zone.clone())
        .unwrap_or_else(|| crate::schedule_timing::zone_name(zone).to_string());
    let schedule_text = shown
        .as_ref()
        .map(|row| row.english.clone())
        .unwrap_or_else(|| schedule.cron_expr.clone());
    let fires = shown
        .as_ref()
        .map(|row| crate::schedule_timing::rows_for(row, 5))
        .unwrap_or_default();
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "cron_expr".to_string(),
            serde_json::json!(schedule.cron_expr),
        );
        obj.insert("timezone".to_string(), serde_json::Value::String(zone_name));
        obj.insert(
            "schedule_text".to_string(),
            serde_json::Value::String(schedule_text.clone()),
        );
        obj.insert(
            "local_time".to_string(),
            serde_json::Value::String(schedule_text),
        );
        obj.insert("next_fires".to_string(), serde_json::Value::Array(fires));
    }
    Ok(value)
}

#[derive(Deserialize)]
struct ScheduleFile {
    #[serde(default)]
    timezone: String,
    #[serde(default)]
    schedules: Vec<CronSchedule>,
}

/// Approve and Deny for a schedule edit. Every other card keeps the four scopes.
pub fn card_options(proposal: &Proposal) -> Vec<ApprovalOption> {
    if is_schedule_edit(proposal) {
        return vec![
            ApprovalOption {
                action: "approve".into(),
                label: "Approve".into(),
            },
            ApprovalOption {
                action: "deny".into(),
                label: "Deny".into(),
            },
        ];
    }
    permission_card_options()
        .into_iter()
        .map(|option| ApprovalOption {
            action: option.action.to_string(),
            label: option.label.to_string(),
        })
        .collect()
}

pub fn is_schedule_edit(proposal: &Proposal) -> bool {
    schedule_call(proposal).is_some_and(|call| is_schedule_write_tool(&call.tool))
}

/// Resolve a schedule card. `None` lets the normal permission path run.
pub async fn try_resolve(
    state: &Arc<AppState>,
    id: &str,
    decision: &str,
) -> Option<axum::response::Response> {
    let hub = state.approval_hub.as_ref()?;
    let proposal = hub.resolver.read_proposal(id).await?;
    if !is_schedule_edit(&proposal) {
        return None;
    }
    let outcome = hub
        .resolver
        .finish_schedule_card(id, decision, DecisionVia::Webui, apply_proposal)
        .await;
    if let ResolveOutcome::Decided { rationale, .. } = &outcome {
        record_reply(state, &proposal, rationale).await;
    }
    Some(http_response(outcome))
}

fn apply_proposal(proposal: &Proposal) -> String {
    match schedule_topology_path() {
        Ok(path) => apply_at(&path, proposal),
        Err(err) => err.to_string(),
    }
}

/// Apply one card against `path` (`topology.toml` or its directory).
pub fn apply_at(path: &Path, proposal: &Proposal) -> String {
    let Some(call) = schedule_call(proposal) else {
        return shape_reply("the card has no schedule tool call");
    };
    match run_edit(path, call) {
        Ok(report) => report.summary(),
        Err(EditError::Shape(detail)) => shape_reply(&detail),
        Err(EditError::Write(err)) => err.to_string(),
    }
}

fn shape_reply(detail: &str) -> String {
    format!("Could not read the schedule edit: {detail}. topology.toml was not changed.")
}

enum EditError {
    Shape(String),
    Write(ScheduleWriteError),
}

fn run_edit(path: &Path, call: &ToolCall) -> Result<ScheduleWriteReport, EditError> {
    match call.tool.as_str() {
        CREATE_SCHEDULE_TOOL_NAME => {
            let schedule = schedule_from_args(&call.args)?;
            create_schedule(path, schedule).map_err(EditError::Write)
        }
        UPDATE_SCHEDULE_TOOL_NAME => {
            let lookup = required_name(&call.args)?;
            let patch = SchedulePatch::from_tool_args(&call.args).map_err(EditError::Shape)?;
            update_schedule(path, &lookup, &patch).map_err(EditError::Write)
        }
        DELETE_SCHEDULE_TOOL_NAME => {
            let name = required_name(&call.args)?;
            delete_schedule(path, &name).map_err(EditError::Write)
        }
        other => Err(EditError::Shape(format!(
            "`{other}` is not a schedule edit"
        ))),
    }
}

fn schedule_from_args(args: &serde_json::Value) -> Result<CronSchedule, EditError> {
    let mut schedule: CronSchedule =
        serde_json::from_value(args.clone()).map_err(|err| EditError::Shape(err.to_string()))?;
    if let Some(new_name) = args.get("new_name").and_then(|value| value.as_str()) {
        let trimmed = new_name.trim();
        if !trimmed.is_empty() {
            schedule.name = trimmed.to_string();
        }
    }
    if schedule.name.trim().is_empty() {
        return Err(EditError::Shape("`name` is empty".into()));
    }
    schedule.require_one_clock().map_err(EditError::Shape)?;
    Ok(schedule)
}

fn required_name(args: &serde_json::Value) -> Result<String, EditError> {
    let name = args
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        return Err(EditError::Shape("`name` is empty".into()));
    }
    Ok(name.to_string())
}

fn schedule_call(proposal: &Proposal) -> Option<&ToolCall> {
    match &proposal.proposed_action {
        ProposedAction::ToolCalls(calls) => calls.first(),
        _ => None,
    }
}

async fn record_reply(state: &AppState, proposal: &Proposal, rationale: &str) {
    let Some(sessions) = &state.chat else {
        return;
    };
    let Some(raw) = proposal.session_id.as_deref() else {
        return;
    };
    let Ok(id) = raw.parse::<Ulid>() else {
        tracing::warn!(
            session = raw,
            "schedule edit reply: session id is not a Ulid"
        );
        return;
    };
    if let Err(err) = sessions.append_note(id, rationale.to_string()).await {
        tracing::warn!(%err, "schedule edit reply was not appended to the chat");
    }
}

fn http_response(outcome: ResolveOutcome) -> axum::response::Response {
    match outcome {
        ResolveOutcome::Decided { action, label, .. } => (
            StatusCode::OK,
            Json(reply("decided", Some(action), Some(&label), None)),
        )
            .into_response(),
        ResolveOutcome::AlreadyDecided { action, label } => (
            StatusCode::CONFLICT,
            Json(reply(
                "already_decided",
                Some(action),
                Some(&label),
                Some(format!("already decided: {label}")),
            )),
        )
            .into_response(),
        ResolveOutcome::NotFound | ResolveOutcome::NotAPermissionRequest => (
            StatusCode::NOT_FOUND,
            Json(reply("not_found", None, None, Some("not found".into()))),
        )
            .into_response(),
        ResolveOutcome::UnknownAction => (
            StatusCode::BAD_REQUEST,
            Json(reply(
                "unknown_decision",
                None,
                None,
                Some("unknown decision".into()),
            )),
        )
            .into_response(),
        ResolveOutcome::Unreadable | ResolveOutcome::SaveFailed => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(reply(
                "save_failed",
                None,
                None,
                Some("could not save the decision".into()),
            )),
        )
            .into_response(),
    }
}

fn reply(
    status: &str,
    decision: Option<&str>,
    label: Option<&str>,
    error: Option<String>,
) -> ApprovalResolveResponse {
    ApprovalResolveResponse {
        status: status.to_string(),
        decision: decision.map(str::to_string),
        label: label.map(str::to_string),
        error,
    }
}

#[cfg(test)]
#[path = "schedule_cards_tests.rs"]
mod tests;
