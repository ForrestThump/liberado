//! `ask_human` tool for interactive ACP coding.
//!
//! Offered when the session may interrupt a person (`Capability::AskHuman`, or a
//! standalone empty grant). Calling it parks the converse loop: the assistant
//! tool-call stays in history without a result, the turn ends so the ACP client
//! can send another `session/prompt`, and that prompt is the answer.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::CapabilitySet;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use serde_json::json;

/// Wire name. Keep it stable — traces and tests match on this string.
pub const ASK_HUMAN_TOOL: &str = "ask_human";

/// Whether this ACP coding session may offer [`ASK_HUMAN_TOOL`].
///
/// An empty grant is standalone (no `policy.toml`): a human is in the editor, so
/// asking is allowed. A declared grant must include `AskHuman`.
pub fn may_ask_human(grant: &CapabilitySet) -> bool {
    grant.capabilities.is_empty() || grant.contains(&liberado_common::Capability::AskHuman)
}

/// Wrap a coding runtime with `ask_human` when `enabled`.
pub fn wrap(inner: Arc<dyn ToolRuntime>, enabled: bool) -> Arc<dyn ToolRuntime> {
    if enabled {
        Arc::new(AskHumanRuntime { inner })
    } else {
        inner
    }
}

struct AskHumanRuntime {
    inner: Arc<dyn ToolRuntime>,
}

fn tool_def() -> ToolDef {
    ToolDef::new(
        ASK_HUMAN_TOOL,
        "Ask the human a question and stop. Their next message is the answer. \
         Use this when you cannot proceed without their decision. Do not keep \
         editing after this call.",
        json!({
            "type": "object",
            "required": ["question"],
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to show the human."
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional choices. The human may still type freely."
                }
            }
        }),
    )
}

fn render_question(call: &ToolInvocation) -> String {
    let question = call
        .arguments
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let question = if question.is_empty() {
        "(no question provided)"
    } else {
        question
    };
    let options = call
        .arguments
        .get("options")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|opts| !opts.is_empty());
    match options {
        Some(opts) => format!("{question}\noptions: {}", opts.join(" | ")),
        None => question.to_string(),
    }
}

#[async_trait]
impl ToolRuntime for AskHumanRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        let mut tools = self.inner.catalog();
        tools.push(tool_def());
        tools
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if call.name == ASK_HUMAN_TOOL {
            return Ok(render_question(call));
        }
        self.inner.invoke(call).await
    }

    fn is_read_only(&self, tool_name: &str) -> bool {
        if tool_name == ASK_HUMAN_TOOL {
            return true;
        }
        self.inner.is_read_only(tool_name)
    }

    fn parks_for_human(&self, tool_name: &str) -> bool {
        tool_name == ASK_HUMAN_TOOL
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::Capability;
    use serde_json::json;

    struct Stub;

    #[async_trait]
    impl ToolRuntime for Stub {
        fn catalog(&self) -> Vec<ToolDef> {
            vec![ToolDef::new("read_file", "r", json!({ "type": "object" }))]
        }
        async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
            Ok(format!("stub:{}", call.name))
        }
    }

    #[test]
    fn empty_grant_may_ask() {
        assert!(may_ask_human(&CapabilitySet::empty()));
    }

    #[test]
    fn declared_grant_needs_the_capability() {
        let mut no = CapabilitySet::empty();
        no.grant(Capability::Read(liberado_common::Zone::named("work")));
        assert!(!may_ask_human(&no));
        let mut yes = CapabilitySet::empty();
        yes.grant(Capability::AskHuman);
        assert!(may_ask_human(&yes));
    }

    #[test]
    fn wrap_disabled_does_not_offer_the_tool() {
        let names: Vec<_> = wrap(Arc::new(Stub), false)
            .catalog()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn wrap_enabled_appends_ask_human_and_parks() {
        let runtime = wrap(Arc::new(Stub), true);
        let names: Vec<_> = runtime.catalog().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"read_file".into()), "{names:?}");
        assert!(names.contains(&ASK_HUMAN_TOOL.into()), "{names:?}");
        assert!(runtime.parks_for_human(ASK_HUMAN_TOOL));
        assert!(!runtime.parks_for_human("read_file"));
    }

    #[tokio::test]
    async fn invoke_renders_question_and_options() {
        let runtime = wrap(Arc::new(Stub), true);
        let out = runtime
            .invoke(&ToolInvocation::new(
                "1",
                ASK_HUMAN_TOOL,
                json!({
                    "question": "Which crate?",
                    "options": ["acp-bridge", "coder-agent"]
                }),
            ))
            .await
            .unwrap();
        assert!(out.contains("Which crate?"), "{out}");
        assert!(out.contains("acp-bridge"), "{out}");
        assert!(out.contains("coder-agent"), "{out}");
    }

    #[tokio::test]
    async fn invoke_forwards_other_tools() {
        let runtime = wrap(Arc::new(Stub), true);
        let out = runtime
            .invoke(&ToolInvocation::new("1", "read_file", json!({})))
            .await
            .unwrap();
        assert_eq!(out, "stub:read_file");
    }

    #[test]
    fn empty_question_is_named_rather_than_blank() {
        let call = ToolInvocation::new("1", ASK_HUMAN_TOOL, json!({ "question": "  " }));
        assert_eq!(render_question(&call), "(no question provided)");
    }
}
