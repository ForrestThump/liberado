//! Read-only automation config reflection — cron schedules and webhook hook names.

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};

use crate::state::AppState;

/// `GET /api/schedules` — configured cron entries from in-memory topology (read-only).
///
/// Does not expose `next_fire` until the cron runtime surfaces it; the field is always `null`.
pub async fn list_schedules(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
                "next_fire": serde_json::Value::Null,
            })
        })
        .collect();
    Json(serde_json::json!({ "schedules": schedules }))
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
