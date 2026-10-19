//! Read-only automation config reflection — cron schedules and webhook hook names.

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};

use crate::state::AppState;

use super::schedule_description::schedule_description;

/// `GET /api/schedules` — configured cron entries from in-memory topology (read-only).
///
/// `next_fire` is the next local reading in `topology.timezone`, or null when
/// the expression does not parse. `description`, `schedule_text`, and `timezone`
/// are additive display fields.
pub async fn list_schedules(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let timezone = state.config.topology.timezone.clone();
    let schedules: Vec<serde_json::Value> = state
        .config
        .topology
        .schedules
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "enabled": s.enabled,
                "cron_expr": s.cron_expr,
                "goal": s.goal,
                "job": s.job,
                "profile": s.profile,
                "next_fire": next_fire_value(&s.cron_expr, &timezone),
                "description": schedule_description(s),
                "schedule_text": liberado_cron::cron_to_english(&s.cron_expr, &timezone),
                "timezone": &timezone,
            })
        })
        .collect();
    Json(serde_json::json!({ "schedules": schedules }))
}

fn next_fire_value(expr: &str, timezone: &str) -> serde_json::Value {
    match liberado_cron::next_fires(expr, timezone, 1) {
        Ok(fires) => match fires.first() {
            Some(fire) => serde_json::Value::String(fire.display()),
            None => serde_json::Value::Null,
        },
        Err(_) => serde_json::Value::Null,
    }
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
