//! `PATCH` and `DELETE /api/profiles/{name}`.
//!
//! Topology stays the operator file. The ledger in the data directory is the live name.
//! Conversations that used the profile are kept. Rename copies the stored grant onto the new
//! name. Delete clears the profile so the next turn uses the default grant.
//!
//! `chat-default` and the Agent Creator profile (`operator`) are refused.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chat_client_contract::ApiError;
use liberado_session::SessionGrant;
use serde::Deserialize;

use crate::state::AppState;
use crate::surfaces::{DeletePlan, ProfileReject, RenamePlan};

#[derive(Deserialize)]
pub struct RenameProfileBody {
    pub name: String,
}

pub async fn rename_profile(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<RenameProfileBody>,
) -> impl IntoResponse {
    let plan = match state.surfaces.profiles.plan_rename(&name, &body.name) {
        Ok(plan) => plan,
        Err(reject) => {
            let to = body.name.trim();
            let shown = match reject {
                ProfileReject::Collision => to,
                ProfileReject::Protected if crate::surfaces::is_protected(to) => to,
                _ => name.as_str(),
            };
            return reject_response(shown, reject);
        }
    };
    let RenamePlan::Apply { original } = plan else {
        return Json(serde_json::json!({
            "name": name,
            "renamed_to": name,
            "conversations": 0,
        }))
        .into_response();
    };
    let to = body.name.trim().to_string();
    if let Err(error) = state.surfaces.profiles.commit_rename(&original, &to) {
        return store_error(error);
    }
    let rebound = match rebind_profile(&state, &name, Some(to.clone())).await {
        Ok(count) => count,
        Err(error) => {
            let _ = state.surfaces.profiles.commit_rename(&original, &name);
            return store_error(error);
        }
    };
    if let Some(chat) = &state.chat {
        chat.rename_profile_route(&name, &to);
    }
    Json(serde_json::json!({
        "name": name,
        "renamed_to": to,
        "conversations": rebound,
    }))
    .into_response()
}

pub async fn delete_profile(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let plan = match state.surfaces.profiles.plan_delete(&name) {
        Ok(plan) => plan,
        Err(reject) => return reject_response(&name, reject),
    };
    let DeletePlan { original } = plan;
    if let Err(error) = state.surfaces.profiles.commit_delete(&original) {
        return store_error(error);
    }
    let kept = match rebind_profile(&state, &name, None).await {
        Ok(count) => count,
        Err(error) => {
            state.surfaces.profiles.revert_delete(&original);
            return store_error(error);
        }
    };
    if let Some(chat) = &state.chat {
        chat.set_profile_route(&name, None);
    }
    Json(serde_json::json!({
        "name": name,
        "deleted": true,
        "conversations_kept": kept,
        "fallback": "default",
    }))
    .into_response()
}

fn reject_response(name: &str, reject: ProfileReject) -> axum::response::Response {
    let status = match reject {
        ProfileReject::Unknown => StatusCode::NOT_FOUND,
        ProfileReject::Protected => StatusCode::FORBIDDEN,
        ProfileReject::Collision => StatusCode::CONFLICT,
        ProfileReject::Invalid(_) => StatusCode::BAD_REQUEST,
    };
    (
        status,
        Json(ApiError {
            error: reject.message(name),
        }),
    )
        .into_response()
}

fn store_error(error: String) -> axum::response::Response {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiError { error })).into_response()
}

/// Point every non-ephemeral session that still names `from` at `next`.
/// `None` clears the profile so the next turn uses the process default grant.
async fn rebind_profile(
    state: &AppState,
    from: &str,
    next: Option<String>,
) -> Result<usize, String> {
    let Some(chat) = &state.chat else {
        return Err("chat is disabled".into());
    };
    let mut count = 0usize;
    for header in state.sessions.list_sessions().await {
        if header.grant.profile.as_deref() != Some(from) {
            continue;
        }
        let grant = match &next {
            Some(name) => renamed_grant(&header.grant, name),
            None => SessionGrant::default(),
        };
        chat.set_profile(header.id, grant)
            .await
            .map_err(|e| e.to_string())?;
        count += 1;
    }
    Ok(count)
}

fn renamed_grant(grant: &SessionGrant, name: &str) -> SessionGrant {
    let mut grant = grant.clone();
    grant.profile = Some(name.to_string());
    grant
}

#[cfg(test)]
#[path = "profiles_http_tests.rs"]
mod tests;
