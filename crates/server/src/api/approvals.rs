//! Human permission decisions over HTTP.
//!
//! `POST /api/approvals/{id}/resolve` is the WebUI's tap. It is not a tool, and it is not
//! `GET`, for the same reason as the profile switch: a web-fetching MCP can only GET
//! loopback. The handler calls [`PermissionResolver::resolve`](liberado_telegram_approvals::PermissionResolver::resolve),
//! the same function the Telegram callback calls.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chat_client_contract::{ApprovalListResponse, ApprovalResolveRequest, ApprovalResolveResponse};
use liberado_common::DecisionVia;
use liberado_telegram_approvals::ResolveOutcome;

use crate::state::AppState;

/// `GET /api/conversations/{id}/approvals` — cards for one chat. Read-only.
pub async fn list_approvals(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let approvals = crate::approvals::conversation_cards(&state.approval_hub, &id).await;
    Json(ApprovalListResponse { approvals })
}

/// `POST /api/approvals/{id}/resolve` — record a human decision. First tap wins.
pub async fn resolve_approval(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<ApprovalResolveRequest>,
) -> impl IntoResponse {
    let Some(hub) = &state.approval_hub else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(reply(
                "unavailable",
                None,
                None,
                Some("approvals unavailable".into()),
            )),
        )
            .into_response();
    };
    let outcome = hub
        .resolver
        .resolve(&id, &body.decision, DecisionVia::Webui)
        .await;
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
#[path = "approvals_http_tests.rs"]
mod http_tests;
