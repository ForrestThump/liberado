//! Dogfood operations over the existing `/api` routes.
//!
//! `create_agent` does not call the face `create_agent` tool. It asks
//! `GET /api/profiles` (fail closed), reuses the oldest matching Agents-shelf
//! row from `GET /api/conversations`, and otherwise `POST /api/conversations`
//! with that profile and title. The POST stamps `surface_mode: agent` when
//! the profile is agent-eligible. This client does not hold the in-process
//! shelf lock, so two overlapping creates can mint two rows. A later call
//! reuses the oldest matching row: parsed RFC3339 `created_at`, then `id`,
//! the same key `pick_oldest` uses on `ConversationHeader` (`DateTime`, then
//! `Ulid`). Wire-text `created_at` order is not used.
//!
//! No function in this module reads an assistant reply and posts it.
//!
//! `list_sessions` defaults to `GET /api/conversations` (foreground chats).
//! That lens already drops background dispatch sessions, so the body stays
//! small. `include_background` reads `GET /api/sessions` instead: every row,
//! including background goal sessions with full goal/grant/result. That
//! transfer is multi-megabyte on a busy daemon. Both paths keep the daemon's
//! order, cap the slim rows they return, and add `total` and `truncated`.
//! `list_agents` is unchanged.

use serde_json::{Value, json};

use crate::client::DaemonClient;
use crate::error::DogfoodError;
use crate::shelf::{
    agent_identity, blank_to_none, header_id, human_message, is_shelf_agent, latest_assistant,
    oldest_match, profile_eligibility, session_id, slim_agent, slim_conversation, slim_session,
    string_list, workspace_report,
};

const DEFAULT_SESSION_LIMIT: u32 = 50;
const MAX_SESSION_LIMIT: u32 = 500;

/// Find or create one Agents-shelf session. Does not send a chat turn.
pub async fn create_agent(
    client: &DaemonClient,
    profile: &str,
    title: Option<&str>,
) -> Result<Value, DogfoodError> {
    let (profile, title) = agent_identity(profile, title)?;
    require_agent_profile(client, &profile).await?;
    if let Some(existing) = find_existing(client, &profile, &title).await? {
        return agent_body(&existing, &profile, true);
    }
    let created = post_agent(client, &profile, &title).await?;
    agent_body(&created, &profile, false)
}

/// Open a normal chat. An agent-eligible profile is refused.
pub async fn start_chat(
    client: &DaemonClient,
    profile: Option<&str>,
    title: Option<&str>,
) -> Result<Value, DogfoodError> {
    let profile = blank_to_none(profile);
    let title = blank_to_none(title);
    if let Some(name) = profile {
        reject_agent_profile(client, name).await?;
    }
    let header = client
        .post_json("/api/conversations", &chat_create_body(profile, title))
        .await?;
    chat_body(&header)
}

/// `POST /api/chat` with the caller's session and message. Shared by
/// `send_human_message` and `continue_session`. The message is the argument;
/// this function does not load history.
pub async fn post_human_turn(
    client: &DaemonClient,
    session: &str,
    message: &str,
) -> Result<Value, DogfoodError> {
    let session = session_id(session)?;
    let message = human_message(message)?;
    client
        .post_json(
            "/api/chat",
            &json!({
                "session": session,
                "message": message,
            }),
        )
        .await
}

/// Latest assistant text from `GET /api/conversations/{id}`. Does not post.
pub async fn read_replies(client: &DaemonClient, session: &str) -> Result<Value, DogfoodError> {
    let session = session_id(session)?;
    let history = get_history(client, session).await?;
    let mut body = json!({
        "session": session,
        "surface_mode": history.get("surface_mode").cloned().unwrap_or(json!("chat")),
        "profile": history.get("profile").cloned().unwrap_or(Value::Null),
        "turn_running": bool_field(&history, "turn_running"),
        "turn_unanswered": bool_field(&history, "turn_unanswered"),
    });
    body["reply"] = latest_assistant(&history).unwrap_or(Value::Null);
    Ok(body)
}

/// Full history document from `GET /api/conversations/{id}`. Does not post.
pub async fn get_history(client: &DaemonClient, session: &str) -> Result<Value, DogfoodError> {
    let session = session_id(session)?;
    client
        .get_json(&format!("/api/conversations/{session}"))
        .await
}

/// Agents-shelf rows from `GET /api/conversations`, creator singleton excluded.
pub async fn list_agents(client: &DaemonClient) -> Result<Value, DogfoodError> {
    let rows = object_rows(client, "/api/conversations").await?;
    let agents: Vec<Value> = rows
        .iter()
        .filter(|row| is_shelf_agent(row))
        .map(slim_agent)
        .collect();
    Ok(json!({ "agents": agents }))
}

/// Slim session rows, capped, with `total` and `truncated`.
///
/// Default is the conversations lens (no `status`/`goal` on those rows).
/// `include_background` switches to `/api/sessions`.
pub async fn list_sessions(
    client: &DaemonClient,
    include_background: bool,
    limit: Option<u32>,
) -> Result<Value, DogfoodError> {
    if include_background {
        listed_sessions(client, "/api/sessions", slim_session, limit).await
    } else {
        listed_sessions(client, "/api/conversations", slim_conversation, limit).await
    }
}

/// `GET /api/status`, unchanged.
pub async fn status(client: &DaemonClient) -> Result<Value, DogfoodError> {
    client.get_json("/api/status").await
}

/// `GET /api/catalog`, unchanged.
pub async fn catalog(client: &DaemonClient) -> Result<Value, DogfoodError> {
    client.get_json("/api/catalog").await
}

/// Read-only workspace-name check against the status tool list.
pub async fn workspace_smoke(client: &DaemonClient) -> Result<Value, DogfoodError> {
    let status = client.get_json("/api/status").await?;
    let names = string_list(status.get("chat_tool_names"));
    Ok(workspace_report(&names))
}

async fn require_agent_profile(client: &DaemonClient, profile: &str) -> Result<(), DogfoodError> {
    let body = client.get_json("/api/profiles").await?;
    if profile_eligibility(&body, profile)? {
        Ok(())
    } else {
        Err(DogfoodError::Invalid(format!(
            "profile `{profile}` is not in the deployment's agent_profiles set \
             (GET /api/profiles agent_eligible is false). Refusing to create a chat row."
        )))
    }
}

async fn reject_agent_profile(client: &DaemonClient, profile: &str) -> Result<(), DogfoodError> {
    let body = client.get_json("/api/profiles").await?;
    if profile_eligibility(&body, profile)? {
        Err(DogfoodError::Invalid(format!(
            "profile `{profile}` is agent-eligible. Use create_agent so the same profile and \
             title reuse one Agents-shelf session."
        )))
    } else {
        Ok(())
    }
}

async fn find_existing(
    client: &DaemonClient,
    profile: &str,
    title: &str,
) -> Result<Option<Value>, DogfoodError> {
    let rows = object_rows(client, "/api/conversations").await?;
    Ok(oldest_match(&rows, profile, title).cloned())
}

async fn post_agent(
    client: &DaemonClient,
    profile: &str,
    title: &str,
) -> Result<Value, DogfoodError> {
    // `agent_creator` is absent on purpose. That flag opens the singleton
    // Agent Creator and ignores profile and title.
    let header = client
        .post_json(
            "/api/conversations",
            &json!({
                "profile": profile,
                "title": title,
            }),
        )
        .await?;
    let mode = header
        .get("surface_mode")
        .and_then(Value::as_str)
        .unwrap_or("");
    if mode != "agent" {
        let id = header
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(DogfoodError::Daemon(format!(
            "POST /api/conversations stamped surface_mode `{mode}` for profile `{profile}`. \
             Expected agent. The row id is {id}."
        )));
    }
    Ok(header)
}

async fn listed_sessions(
    client: &DaemonClient,
    path: &str,
    slim: fn(&Value) -> Value,
    limit: Option<u32>,
) -> Result<Value, DogfoodError> {
    let rows = object_rows(client, path).await?;
    Ok(capped_session_list(rows.iter().map(slim).collect(), limit))
}

fn capped_session_list(mut sessions: Vec<Value>, limit: Option<u32>) -> Value {
    let total = sessions.len();
    let cap = limit
        .unwrap_or(DEFAULT_SESSION_LIMIT)
        .min(MAX_SESSION_LIMIT) as usize;
    let truncated = total > cap;
    sessions.truncate(cap);
    json!({
        "sessions": sessions,
        "total": total,
        "truncated": truncated,
    })
}

async fn object_rows(client: &DaemonClient, path: &str) -> Result<Vec<Value>, DogfoodError> {
    let body = client.get_json(path).await?;
    let rows = body
        .as_array()
        .ok_or_else(|| DogfoodError::Daemon(format!("{path} was not a JSON array")))?;
    Ok(rows.iter().filter(|row| row.is_object()).cloned().collect())
}

fn agent_body(header: &Value, profile: &str, reused: bool) -> Result<Value, DogfoodError> {
    let id = header_id(header)?;
    let title = header
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(profile);
    Ok(json!({
        "conversation_id": id,
        "profile": profile,
        "title": title,
        "surface_mode": "agent",
        "reused": reused,
    }))
}

fn chat_body(header: &Value) -> Result<Value, DogfoodError> {
    let id = header_id(header)?;
    Ok(json!({
        "conversation_id": id,
        "title": header.get("title").cloned().unwrap_or(Value::Null),
        "profile": header
            .get("grant")
            .and_then(|grant| grant.get("profile"))
            .cloned()
            .unwrap_or(Value::Null),
        "surface_mode": header.get("surface_mode").cloned().unwrap_or(json!("chat")),
    }))
}

fn chat_create_body(profile: Option<&str>, title: Option<&str>) -> Value {
    let mut body = serde_json::Map::new();
    if let Some(profile) = profile {
        body.insert("profile".into(), Value::String(profile.to_owned()));
    }
    if let Some(title) = title {
        body.insert("title".into(), Value::String(title.to_owned()));
    }
    Value::Object(body)
}

fn bool_field(value: &Value, name: &str) -> bool {
    value.get(name).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
