//! Read-only grant view for one conversation.
//!
//! `GET /api/conversations/{id}/grants` reports the agent grant and the session
//! grant. It does not change either one. Revoke is a later change.

use serde::{Deserialize, Serialize};

/// Which side the next turn runs under.
///
/// A named session profile is authoritative even when its grant is empty.
/// No profile means the process-wide agent grant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantSource {
    #[default]
    Agent,
    Session,
}

/// One zone the grant names, plus the policy write class for that zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneGrant {
    pub name: String,
    /// `read`, `write`, or `read_summary`.
    pub access: String,
    /// `agent_writable`, `human_only`, `proposal_only`, or `shared`.
    pub write_class: String,
}

/// Tool, MCP, and zone authority for one side (the agent, or the session).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AuthorityGrants {
    /// Whole MCP servers (`ExecuteMcp`). Not expanded into tool names.
    pub mcps: Vec<String>,
    /// Individual `mcp:tool` grants (`ExecuteTool`).
    pub tools: Vec<String>,
    pub zones: Vec<ZoneGrant>,
    #[serde(default)]
    pub ask_human: bool,
}

impl AuthorityGrants {
    /// True when this side names no tool, MCP, zone, or ask-human grant.
    pub fn is_empty(&self) -> bool {
        self.mcps.is_empty() && self.tools.is_empty() && self.zones.is_empty() && !self.ask_human
    }
}

/// Reply from `GET /api/conversations/{id}/grants`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationGrantsResponse {
    pub id: String,
    /// Named session profile. Absent when the conversation uses the agent grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// `agent` or `session`.
    #[serde(default)]
    pub effective: GrantSource,
    /// Policy component of [`agent`](Self::agent). Today this is `main-agent`.
    #[serde(default = "main_agent_component")]
    pub agent_component: String,
    pub agent: AuthorityGrants,
    pub session: AuthorityGrants,
    /// Connected tools the effective grant permits. Built-in face tools are not
    /// capability grants, so they are absent here.
    #[serde(default)]
    pub callable: Vec<String>,
}

fn main_agent_component() -> String {
    "main-agent".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ConversationGrantsResponse {
        ConversationGrantsResponse {
            id: "01CONV".into(),
            profile: Some("research".into()),
            effective: GrantSource::Session,
            agent_component: "main-agent".into(),
            agent: AuthorityGrants {
                mcps: vec!["tasks-mcp".into()],
                tools: Vec::new(),
                zones: vec![ZoneGrant {
                    name: "tasks".into(),
                    access: "read".into(),
                    write_class: "proposal_only".into(),
                }],
                ask_human: true,
            },
            session: AuthorityGrants {
                mcps: Vec::new(),
                tools: vec!["turbovault:read_note".into()],
                zones: vec![ZoneGrant {
                    name: "notes".into(),
                    access: "write".into(),
                    write_class: "agent_writable".into(),
                }],
                ask_human: false,
            },
            callable: vec!["tasks-mcp:add".into()],
        }
    }

    #[test]
    fn grants_response_round_trips() {
        let value = serde_json::to_value(sample()).unwrap();
        assert_eq!(value["effective"], "session");
        assert_eq!(value["agent_component"], "main-agent");
        assert_eq!(value["session"]["tools"][0], "turbovault:read_note");
        assert!(value.get("revoke").is_none());
        let back: ConversationGrantsResponse = serde_json::from_value(value).unwrap();
        assert_eq!(back, sample());
    }

    #[test]
    fn a_missing_effective_field_reads_as_the_agent() {
        let back: ConversationGrantsResponse = serde_json::from_value(serde_json::json!({
            "id": "01CONV",
            "agent": { "mcps": [], "tools": [], "zones": [] },
            "session": { "mcps": [], "tools": [], "zones": [] }
        }))
        .unwrap();
        assert_eq!(back.effective, GrantSource::Agent);
        assert_eq!(back.agent_component, "main-agent");
        assert!(back.callable.is_empty());
        assert!(back.profile.is_none());
    }

    #[test]
    fn an_empty_authority_is_empty_until_any_grant_appears() {
        assert!(AuthorityGrants::default().is_empty());
        let grants = AuthorityGrants {
            ask_human: true,
            ..AuthorityGrants::default()
        };
        assert!(!grants.is_empty());
    }
}
