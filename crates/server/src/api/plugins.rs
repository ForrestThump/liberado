//! `PUT /api/plugins/{id}` — chat access for one MCP server.
//!
//! The switch narrows the `main-agent` grant. It cannot grant a server policy did not grant.
//! `visible_to_dispatcher` is left alone. The mask is a data-dir ledger, not a `topology.toml` edit.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chat_client_contract::ApiError;
use serde::Deserialize;

use crate::state::AppState;
use crate::surfaces::PluginReject;

#[derive(Deserialize)]
pub struct PluginEnabledBody {
    pub enabled: bool,
}

pub async fn set_plugin_enabled(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<PluginEnabledBody>,
) -> impl IntoResponse {
    if !state.catalog.descriptors().iter().any(|mcp| mcp.name == id) {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: format!("unknown MCP server '{id}'"),
            }),
        )
            .into_response();
    }
    let policy_grants = state.main_agent_capabilities.grants_mcp(&id);
    match state
        .surfaces
        .plugins
        .set_chat_enabled(&id, body.enabled, policy_grants)
    {
        Ok(visible) => Json(serde_json::json!({
            "name": id,
            "enabled": body.enabled,
            "visible_to_main_agent": visible,
        }))
        .into_response(),
        Err(PluginReject::NotGranted) => (
            StatusCode::CONFLICT,
            Json(ApiError {
                error: PluginReject::NotGranted.message(&id),
            }),
        )
            .into_response(),
        Err(PluginReject::Store(error)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error })).into_response()
        }
    }
}

#[cfg(test)]
#[path = "plugins_http_tests.rs"]
mod tests;
