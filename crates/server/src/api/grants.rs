//! Read the grant stored on a session header.
//!
//! `GET /api/conversations/{id}/grant` returns that header. It does not
//! re-resolve policy and it does not write.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chat_client_contract::{ApiError, HeaderGrantResponse, SurfaceMode};
use liberado_conversation_store::Ulid;
use liberado_session::SessionGrant;
use liberado_session_store::{AgentProfiles, SessionHeader};

use crate::state::AppState;

/// `GET /api/conversations/{id}/grant` — current header grant. Read-only.
pub async fn get_header_grant(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Ulid>,
) -> impl IntoResponse {
    match state.sessions.session(id).await {
        Some(header) => {
            Json(grant_body(id, &header, state.sessions.agent_profiles())).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "conversation not found".into(),
            }),
        )
            .into_response(),
    }
}

fn grant_body(id: Ulid, header: &SessionHeader, profiles: &AgentProfiles) -> HeaderGrantResponse {
    let projected = header.to_conversation_header_with(profiles);
    HeaderGrantResponse {
        id: id.to_string(),
        surface_mode: wire_surface(projected.surface_mode),
        grant: grant_json(&projected.grant),
    }
}

fn grant_json(grant: &SessionGrant) -> serde_json::Value {
    serde_json::to_value(grant).unwrap_or_else(|_| serde_json::json!({}))
}

fn wire_surface(mode: liberado_conversation_store::SurfaceMode) -> SurfaceMode {
    match mode {
        liberado_conversation_store::SurfaceMode::Agent => SurfaceMode::Agent,
        liberado_conversation_store::SurfaceMode::Chat => SurfaceMode::Chat,
    }
}

#[cfg(test)]
#[path = "grants_http_tests.rs"]
mod http_tests;
