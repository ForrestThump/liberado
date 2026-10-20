//! Read-only automation config reflection — cron schedules and webhook hook names.

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};

use crate::state::AppState;

use super::schedule_description::schedule_description;

/// `GET /api/schedules` — configured cron entries from in-memory topology (read-only).
///
/// `next_fire` is the next local reading. A structured row, and a `cron_expr`
/// row that sets `timezone`, are evaluated in that zone. A `cron_expr` row
/// without `timezone` stays UTC and is displayed in `topology.timezone`.
/// `description`, `schedule_text`, and `timezone` are additive display fields.
pub async fn list_schedules(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let timezone = state.config.topology.timezone.clone();
    let schedules: Vec<serde_json::Value> = state
        .config
        .topology
        .schedules
        .iter()
        .map(|s| schedule_json(s, &timezone))
        .collect();
    Json(serde_json::json!({ "schedules": schedules }))
}

fn schedule_json(schedule: &liberado_config::CronSchedule, topology_tz: &str) -> serde_json::Value {
    let shown = crate::schedule_timing::show(schedule, topology_tz).ok();
    let (next_fire, schedule_text, zone) = match &shown {
        Some(shown) => (
            next_from_shown(shown),
            shown.english.clone(),
            shown.zone.clone(),
        ),
        None => (
            serde_json::Value::Null,
            schedule.cron_expr.clone(),
            topology_tz.to_string(),
        ),
    };
    serde_json::json!({
        "name": schedule.name,
        "enabled": schedule.enabled,
        "cron_expr": schedule.cron_expr,
        "days": schedule.days,
        "at": schedule.at,
        "goal": schedule.goal,
        "job": schedule.job,
        "profile": schedule.profile,
        "next_fire": next_fire,
        "description": schedule_description(schedule),
        "schedule_text": schedule_text,
        "timezone": zone,
    })
}

fn next_from_shown(shown: &crate::schedule_timing::ShownSchedule) -> serde_json::Value {
    let rows = crate::schedule_timing::rows_for(shown, 1);
    rows.first()
        .and_then(|row| row.get("display"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// `GET /api/hooks` — webhook hook names from topology. Never returns secrets or `secret_ref`.
pub async fn list_hooks(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let hooks: Vec<serde_json::Value> = state
        .config
        .topology
        .hooks
        .iter()
        .map(|h| {
            serde_json::json!({
                "name": h.name,
                "enabled": h.enabled,
            })
        })
        .collect();
    Json(serde_json::json!({ "hooks": hooks }))
}

#[cfg(test)]
#[path = "automations_http_tests.rs"]
mod http_tests;
