//! Privileged face tool: create a long-lived Agent-shelf chat (not `delegate`).
//!
//! The face agent *requests* create; the dispatcher/create path remains the sole
//! grantor via [`ChatSessions::create_with_grant`](crate::ChatSessions::create_with_grant).
//! Child capability is only whatever `Config::resolve_session_profile` returns —
//! callers cannot pass custom tool lists. Distinct from `delegate` (GoalSessionHub
//! dispatch-domain jobs).

use async_trait::async_trait;
use liberado_provider::ToolDef;
use serde_json::json;

/// Tool name the privileged face agent calls to open a specialist Agents-shelf chat.
pub const CREATE_AGENT_TOOL_NAME: &str = "create_agent";

/// Result returned to the model as JSON after a successful create.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateAgentResult {
    pub conversation_id: String,
    pub profile: String,
    pub title: Option<String>,
}

impl CreateAgentResult {
    pub fn to_json(&self) -> String {
        let mut body = json!({
            "conversation_id": self.conversation_id,
            "surface_mode": "agent",
            "profile": self.profile,
        });
        if let Some(title) = &self.title {
            body["title"] = json!(title);
        }
        body.to_string()
    }
}

/// Create path used by the face `create_agent` tool. Implemented by [`crate::ChatSessions`].
#[async_trait]
pub trait AgentSpawner: Send + Sync {
    async fn spawn_agent(
        &self,
        profile: &str,
        title: Option<String>,
    ) -> Result<CreateAgentResult, String>;
}

pub fn create_agent_tool_def() -> ToolDef {
    ToolDef::new(
        CREATE_AGENT_TOOL_NAME,
        "Create a long-lived specialist Agent chat on the Agents shelf (Grok-Bot-style peer \
         context). Pass an agent-eligible chat profile name. The eligible set is the deployment's \
         `[chat] agent_profiles` (default: `coding`, `life`, `researcher`, `operator`); the create \
         is refused with a clear error if the name is not in the deployment's set. Returns the \
         new conversation id — it is NOT a subagent job and does not await a goal. Prefer this \
         over delegate when the human wants an ongoing specialist conversation rather than a \
         one-shot dispatch.",
        json!({
            "type": "object",
            "properties": {
                "profile": {
                    "type": "string",
                    "description": "Agent-eligible chat profile name (e.g. coding, life, researcher, operator — the actual eligible set is deployment-tuned via [chat] agent_profiles)."
                },
                "title": {
                    "type": "string",
                    "description": "Optional sidebar title for the new agent conversation."
                }
            },
            "required": ["profile"]
        }),
    )
}

#[derive(Debug)]
pub(super) struct CreateAgentArgs {
    pub profile: String,
    pub title: Option<String>,
}

/// Parse the model-supplied arguments. Validates shape only — `profile` is non-empty
/// after trimming. The policy check (is this profile in the deployment's `[chat]
/// agent_profiles` set?) lives in [`ChatSessions::create_agent_chat`](crate::ChatSessions::create_agent_chat)
/// so the parser has no opinion on what counts as "agent-eligible": the answer is
/// deployment-tunable, and the parser would otherwise be reading a default set the
/// deployment did not pick.
pub(super) fn parse_create_agent_args(
    arguments: &serde_json::Value,
) -> Result<CreateAgentArgs, String> {
    let profile = arguments
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if profile.is_empty() {
        return Err("create_agent requires a non-empty `profile` string".into());
    }
    let title = arguments
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Ok(CreateAgentArgs {
        profile: profile.to_owned(),
        title,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_conversation_store::is_agent_creator_profile;

    #[test]
    fn parse_validates_shape_only_not_policy() {
        // Non-empty after trim: ok, even for profiles the parser has no opinion on.
        let ok = parse_create_agent_args(&json!({"profile": "designer"})).unwrap();
        assert_eq!(ok.profile, "designer");
        assert_eq!(ok.title, None);
        // Title is trimmed; empty / whitespace-only title drops to None.
        let ok = parse_create_agent_args(&json!({"profile": "coding", "title": "  X  "})).unwrap();
        assert_eq!(ok.profile, "coding");
        assert_eq!(ok.title.as_deref(), Some("X"));
        let ok = parse_create_agent_args(&json!({"profile": "coding", "title": "   "})).unwrap();
        assert_eq!(ok.title, None);
    }

    #[test]
    fn parse_rejects_empty_profile() {
        let err = parse_create_agent_args(&json!({"profile": ""})).unwrap_err();
        assert!(err.contains("non-empty"), "{err}");
        let err = parse_create_agent_args(&json!({"profile": "   "})).unwrap_err();
        assert!(err.contains("non-empty"), "{err}");
        let err = parse_create_agent_args(&json!({})).unwrap_err();
        assert!(err.contains("non-empty"), "{err}");
    }

    #[test]
    fn creator_gate_allows_default_face_and_operator_only() {
        assert!(is_agent_creator_profile(None));
        assert!(is_agent_creator_profile(Some("operator")));
        assert!(!is_agent_creator_profile(Some("coding")));
        assert!(!is_agent_creator_profile(Some("life")));
        assert!(!is_agent_creator_profile(Some("researcher")));
    }

    #[test]
    fn result_json_shape() {
        let s = CreateAgentResult {
            conversation_id: "01ABC".into(),
            profile: "coding".into(),
            title: Some("Bot".into()),
        }
        .to_json();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["conversation_id"], "01ABC");
        assert_eq!(v["surface_mode"], "agent");
        assert_eq!(v["profile"], "coding");
        assert_eq!(v["title"], "Bot");
    }
}
