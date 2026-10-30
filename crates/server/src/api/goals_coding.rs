//! Coding start and rewind paths for the goals API.
//!
//! Split from `goals.rs` so that file stays within its module-health cyclomatic baseline.
//! The off build refuses a coding domain. The on build authorizes the workspace and restores
//! a shadow-git checkpoint.

use axum::{Json, http::StatusCode, response::IntoResponse};

use chat_client_contract::ApiError;

#[cfg(feature = "coding")]
use crate::state::AppState;

/// 400 whose body names the missing `coding` feature.
#[cfg(not(feature = "coding"))]
pub(super) fn missing_coding_feature() -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: crate::coding_notice::FEATURE_REQUIRED.to_string(),
        }),
    )
        .into_response()
}

/// Strip Windows extended-length prefixes (`\\?\C:\…`, `\\?\UNC\…`) so paths on the wire are
/// usable by git and readable in session records.
#[cfg(feature = "coding")]
pub(in crate::api) fn strip_windows_extended_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix(r"UNC\") {
            return format!(r"\\{unc}");
        }
        return rest.to_string();
    }
    s.into_owned()
}

/// Coding project authorization. `Some` is the HTTP response that refuses the start.
#[cfg(feature = "coding")]
pub(super) fn prepare_coding_goal(
    state: &AppState,
    goal: &mut liberado_session::GoalSpec,
) -> Option<axum::response::Response> {
    let mut payload = match liberado_coder_agent::CodingGoalPayload::parse(&goal.payload) {
        Ok(payload) => payload,
        Err(error) => {
            return Some((StatusCode::BAD_REQUEST, Json(ApiError { error })).into_response());
        }
    };
    let project = payload.project().map(str::to_string);
    let workspace_root = payload.workspace_root().map(str::to_string);
    match state
        .config
        .authorize_coding_workspace(project.as_deref(), workspace_root.as_deref())
    {
        Ok(liberado_config::CodingWorkspaceAuth::Ephemeral) => {
            goal.payload = payload.into_value();
        }
        Ok(liberado_config::CodingWorkspaceAuth::Project { name, root }) => {
            // Strip Windows `\\?\` extended prefixes so session records and git tools see a
            // plain drive path (dogfood finding #1 residual).
            let root_s = strip_windows_extended_path(&root);
            payload.set_authorized_workspace(name.clone(), root_s);
            goal.payload = payload.into_value();
            // Inject ship preflight from topology when the client did not supply steps.
            // Pack still applies liberado built-in defaults when project is "liberado".
            let payload = goal
                .payload
                .as_object_mut()
                .expect("CodingGoalPayload always serializes to an object");
            if payload
                .get("preflight")
                .and_then(|v| v.get("steps"))
                .is_none()
                && let Some(proj) = state.config.project_by_name(&name)
                && let Some(preflight) = proj.ship_preflight_payload()
            {
                payload.insert("preflight".into(), preflight);
            }
        }
        Err(e) => {
            return Some(
                (
                    StatusCode::FORBIDDEN,
                    Json(ApiError {
                        error: e.to_string(),
                    }),
                )
                    .into_response(),
            );
        }
    }
    None
}

/// Resolve the rewind workspace: a durable session worktree wins, the payload's
/// `workspace_root` is the fallback. Pure so the two failure messages are unit-tested.
#[cfg(feature = "coding")]
pub(in crate::api) fn rewind_workspace(
    durable: Option<std::path::PathBuf>,
    payload_root: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    match durable {
        Some(path) if path.exists() => Ok(path),
        Some(_) => payload_root.map(std::path::PathBuf::from).ok_or_else(|| {
            "coding session has no workspace_root and no durable session worktree — cannot rewind"
                .to_string()
        }),
        None => payload_root
            .map(std::path::PathBuf::from)
            .ok_or_else(|| "coding session has no workspace_root in payload".to_string()),
    }
}

/// Latest-or-explicit checkpoint for a rewind: an explicit id that names a checkpoint event wins
/// (with the event's label + tree hash); otherwise the most recent checkpoint event is used; a
/// missing explicit id falls back to the id alone with an "explicit" label.
#[cfg(feature = "coding")]
pub(in crate::api) fn rewind_checkpoint(
    events: &[liberado_session::SessionEvent],
    want: Option<&str>,
) -> Result<(String, String, String), String> {
    use liberado_session::SessionEventKind;
    if let Some(id) = want {
        let from_ev = events.iter().rev().find_map(|e| match &e.kind {
            SessionEventKind::Checkpoint {
                id: cid,
                label,
                tree_hash,
            } if cid == id => Some((cid.clone(), label.clone(), tree_hash.clone())),
            _ => None,
        });
        return Ok(from_ev.unwrap_or_else(|| (id.to_string(), "explicit".into(), String::new())));
    }
    events
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            SessionEventKind::Checkpoint {
                id,
                label,
                tree_hash,
            } => Some((id.clone(), label.clone(), tree_hash.clone())),
            _ => None,
        })
        .ok_or_else(|| "no checkpoint events on this session — cannot rewind".to_string())
}

/// Restore workspace files from a shadow-git checkpoint. Coding sessions only.
#[cfg(feature = "coding")]
pub(super) async fn restore_checkpoint(
    id: &str,
    payload_root: Option<&str>,
    events: &[liberado_session::SessionEvent],
    want: Option<&str>,
) -> axum::response::Response {
    let workspace = match rewind_workspace(
        liberado_coder_agent::durable_session_workspace(id),
        payload_root,
    ) {
        Ok(w) => w,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(ApiError { error })).into_response();
        }
    };
    let (cp_id, label, tree_hash) = match rewind_checkpoint(events, want) {
        Ok(cp) => cp,
        Err(error) => {
            return (StatusCode::CONFLICT, Json(ApiError { error })).into_response();
        }
    };

    let sg = match liberado_coder_agent::ShadowGit::open_or_init(&workspace, id) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: format!("open shadow-git: {e}"),
                }),
            )
                .into_response();
        }
    };
    if let Err(e) = sg.restore(&cp_id).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: format!("restore checkpoint: {e}"),
            }),
        )
            .into_response();
    }
    tracing::info!(
        session = %id,
        checkpoint = %cp_id,
        label = %label,
        "goals_rewind: restored workspace files"
    );
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "session_id": id,
            "checkpoint_id": cp_id,
            "label": label,
            "tree_hash": tree_hash,
            "restored": true,
        })),
    )
        .into_response()
}
