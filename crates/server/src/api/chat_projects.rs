//! Chat projects: topology coding roots plus projects a person created.
//!
//! `GET /api/projects` stays the coding picker. These routes are the WebUI list.
//! A user project has no root and no write class, so it cannot authorize `/goal`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use liberado_common::WriteClass;
use liberado_session_store::{ProjectWriteError, UserProject};
use serde::{Deserialize, Serialize};

use chat_client_contract::ApiError;

use crate::state::AppState;

#[derive(Debug, Serialize)]
struct ChatProjectRow {
    id: String,
    name: String,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    write_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateChatProject {
    pub name: String,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PatchChatProject {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "tri_state")]
    pub instructions: Option<Option<String>>,
}

fn tri_state<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(deserializer)?))
}

/// A name the chat may store: an enabled topology project, or a user project id.
///
/// Blank text clears. An unknown key is an error. A user project id is not a
/// coding root; [`liberado_bootstrap::Config::project_by_name`] still ignores it.
pub(crate) async fn accepted_project(
    state: &AppState,
    raw: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let name = raw.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if state.config.project_by_name(name).is_some()
        || state.sessions.user_project(name).await.is_some()
    {
        return Ok(Some(name.to_string()));
    }
    Err(format!("unknown project: {name}"))
}

/// `GET /api/chat-projects` — topology projects, then user projects.
pub async fn list_chat_projects(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match merged_projects(&state).await {
        Ok(projects) => Json(serde_json::json!({ "projects": projects })).into_response(),
        Err(error) => write_error(error),
    }
}

/// `POST /api/chat-projects` — create a project that cannot grant a coding root.
pub async fn create_chat_project(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateChatProject>,
) -> Response {
    match state
        .sessions
        .create_user_project(&body.name, body.instructions.as_deref())
        .await
    {
        Ok(project) => (StatusCode::CREATED, Json(user_row(project))).into_response(),
        Err(error) => write_error(error),
    }
}

/// `PATCH /api/chat-projects/{id}` — rename a user project, or edit its instructions.
pub async fn patch_chat_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<PatchChatProject>,
) -> Response {
    if state.config.project_by_name(&id).is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "topology projects are declared in topology.toml".into(),
            }),
        )
            .into_response();
    }
    if body.name.is_none() && body.instructions.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "nothing to update".into(),
            }),
        )
            .into_response();
    }
    let instructions = body.instructions.as_ref().map(|value| value.as_deref());
    match state
        .sessions
        .update_user_project(&id, body.name.as_deref(), instructions)
        .await
    {
        Ok(project) => Json(user_row(project)).into_response(),
        Err(error) => write_error(error),
    }
}

/// `DELETE /api/chat-projects/{id}` — drop the project and detach its chats.
pub async fn delete_chat_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    if state.config.project_by_name(&id).is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "topology projects are declared in topology.toml".into(),
            }),
        )
            .into_response();
    }
    match state.sessions.delete_user_project(&id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => write_error(error),
    }
}

/// `GET /api/chat-projects/{id}/chats` — title and last activity, newest first.
pub async fn list_project_chats(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    if !project_exists(&state, &id).await {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "project not found".into(),
            }),
        )
            .into_response();
    }
    let chats = state.sessions.project_chats(&id).await;
    Json(serde_json::json!({ "chats": chats })).into_response()
}

async fn project_exists(state: &AppState, id: &str) -> bool {
    state.config.project_by_name(id).is_some() || state.sessions.user_project(id).await.is_some()
}

async fn merged_projects(state: &AppState) -> Result<Vec<ChatProjectRow>, ProjectWriteError> {
    let mut rows: Vec<ChatProjectRow> = state
        .config
        .enabled_projects()
        .into_iter()
        .map(|project| ChatProjectRow {
            id: project.name.clone(),
            name: project.name.clone(),
            source: "topology",
            root: Some(project.root.display().to_string()),
            write_class: Some(write_class_name(project.write_class).to_string()),
            instructions: None,
        })
        .collect();
    let mut user = state.sessions.list_user_projects().await?;
    user.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    rows.extend(user.into_iter().map(user_row));
    Ok(rows)
}

fn user_row(project: UserProject) -> ChatProjectRow {
    ChatProjectRow {
        id: project.id,
        name: project.name,
        source: "user",
        root: None,
        write_class: None,
        instructions: project.instructions,
    }
}

fn write_class_name(class: WriteClass) -> &'static str {
    match class {
        WriteClass::HumanOnly => "human_only",
        WriteClass::AgentWritable => "agent_writable",
        WriteClass::ProposalOnly => "proposal_only",
        WriteClass::Shared => "shared",
    }
}

fn write_error(error: ProjectWriteError) -> Response {
    let status = match error {
        ProjectWriteError::NotFound => StatusCode::NOT_FOUND,
        ProjectWriteError::Invalid(_) => StatusCode::BAD_REQUEST,
        ProjectWriteError::Unavailable(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiError {
            error: error.to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
#[path = "chat_projects_http_tests.rs"]
mod http_tests;
