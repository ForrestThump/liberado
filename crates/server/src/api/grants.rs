//! `GET /api/conversations/{id}/grants` — read-only tool, MCP, and zone grants.
//!
//! The next turn's authority is the same rule `ChatSessions::turn_settings` already
//! uses: a named profile is the session grant, even when that grant is empty. No
//! profile means the process-wide `main-agent` grant. This handler only reads that
//! result. It does not resolve a profile again, and it does not revoke anything.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chat_client_contract::{
    ApiError, AuthorityGrants, ConversationGrantsResponse, GrantSource, ZoneGrant,
};
use liberado_common::{Capability, CapabilitySet, WriteClass};
use liberado_conversation_store::Ulid;

use super::chat::CHAT_DISABLED_HINT;
use crate::state::AppState;

/// `GET /api/conversations/{id}/grants`.
pub async fn conversation_grants(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Ulid>,
) -> impl IntoResponse {
    if state.chat.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: CHAT_DISABLED_HINT.into(),
            }),
        )
            .into_response();
    }
    let Some(header) = state.sessions.session(id).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "conversation not found".into(),
            }),
        )
            .into_response();
    };
    let profile = header.grant.profile.clone();
    let policy = &state.config.policy;
    Json(project_conversation_grants(
        &id.to_string(),
        profile.as_deref(),
        &state.main_agent_capabilities,
        &header.grant.capabilities,
        &state.chat_tool_names,
        |zone| write_class_label(policy.write_class(zone)),
    ))
    .into_response()
}

/// Build the wire view. `session_stored` is the header grant. It is authority only
/// when `profile` is `Some`.
pub(super) fn project_conversation_grants(
    id: &str,
    profile: Option<&str>,
    agent: &CapabilitySet,
    session_stored: &CapabilitySet,
    connected_tools: &[String],
    write_class_of: impl Fn(&str) -> String,
) -> ConversationGrantsResponse {
    let effective = effective_source(profile);
    let session = session_view(profile, session_stored, &write_class_of);
    let agent_view = project_authority(agent, &write_class_of);
    let effective_caps = match effective {
        GrantSource::Session => session_stored,
        GrantSource::Agent => agent,
    };
    ConversationGrantsResponse {
        id: id.to_string(),
        profile: profile.map(str::to_string),
        effective,
        agent_component: "main-agent".to_string(),
        agent: agent_view,
        session,
        callable: callable_tools(effective_caps, connected_tools),
    }
}

fn effective_source(profile: Option<&str>) -> GrantSource {
    match profile {
        Some(_) => GrantSource::Session,
        None => GrantSource::Agent,
    }
}

fn session_view(
    profile: Option<&str>,
    stored: &CapabilitySet,
    write_class_of: &impl Fn(&str) -> String,
) -> AuthorityGrants {
    match profile {
        Some(_) => project_authority(stored, write_class_of),
        None => AuthorityGrants::default(),
    }
}

fn project_authority(
    caps: &CapabilitySet,
    write_class_of: &impl Fn(&str) -> String,
) -> AuthorityGrants {
    let mut zones = Vec::new();
    for cap in &caps.capabilities {
        if let Some(zone) = zone_grant(cap, write_class_of) {
            zones.push(zone);
        }
    }
    AuthorityGrants {
        mcps: caps.granted_mcps(),
        tools: caps.granted_tools(),
        zones,
        ask_human: caps.grants_ask_human(),
    }
}

fn zone_grant(cap: &Capability, write_class_of: &impl Fn(&str) -> String) -> Option<ZoneGrant> {
    let (access, name) = match cap {
        Capability::Read(zone) => ("read", zone.name()),
        Capability::Write(zone) => ("write", zone.name()),
        Capability::ReadSummary(zone) => ("read_summary", zone.name()),
        _ => return None,
    };
    Some(ZoneGrant {
        name: name.to_string(),
        access: access.to_string(),
        write_class: write_class_of(name),
    })
}

fn callable_tools(caps: &CapabilitySet, connected: &[String]) -> Vec<String> {
    connected
        .iter()
        .filter(|name| caps.grants_tool(name))
        .cloned()
        .collect()
}

fn write_class_label(class: WriteClass) -> String {
    match class {
        WriteClass::HumanOnly => "human_only",
        WriteClass::AgentWritable => "agent_writable",
        WriteClass::ProposalOnly => "proposal_only",
        WriteClass::Shared => "shared",
    }
    .to_string()
}

#[cfg(test)]
#[path = "grants_tests.rs"]
mod tests;
