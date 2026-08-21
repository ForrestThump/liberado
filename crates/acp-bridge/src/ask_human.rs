//! `ask_human` tool for interactive ACP coding.
//!
//! Offered when the session may interrupt a person (`Capability::AskHuman`, or a
//! standalone empty grant).
//!
//! With option buttons and an ACP client, the question is `session/request_permission`
//! and the chosen name is the tool result. Without options (or without a client),
//! the call parks: the next `session/prompt` is the answer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use liberado_common::CapabilitySet;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use serde_json::json;

use crate::permission::PermissionAsk;

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
#[cfg(test)]
pub fn wrap(inner: Arc<dyn ToolRuntime>, enabled: bool) -> Arc<dyn ToolRuntime> {
    wrap_with_client(inner, enabled, None)
}

/// Same as [`wrap`], and optioned questions wait on the ACP client chooser.
pub fn wrap_with_client(
    inner: Arc<dyn ToolRuntime>,
    enabled: bool,
    client: Option<(Arc<dyn PermissionAsk>, String)>,
) -> Arc<dyn ToolRuntime> {
    if enabled {
        Arc::new(AskHumanRuntime {
            inner,
            client,
            parked: AtomicBool::new(true),
        })
    } else {
        inner
    }
}

struct AskHumanRuntime {
    inner: Arc<dyn ToolRuntime>,
    client: Option<(Arc<dyn PermissionAsk>, String)>,
    parked: AtomicBool,
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

fn question_text(call: &ToolInvocation) -> String {
    let question = call
        .arguments
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if question.is_empty() {
        "(no question provided)".to_string()
    } else {
        question.to_string()
    }
}

fn option_list(call: &ToolInvocation) -> Vec<String> {
    call.arguments
        .get("options")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn render_question(call: &ToolInvocation) -> String {
    let question = question_text(call);
    let opts = option_list(call);
    if opts.is_empty() {
        question
    } else {
        format!("{question}\noptions: {}", opts.join(" | "))
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
            let options = option_list(call);
            if !options.is_empty()
                && let Some((ask, session_id)) = &self.client
            {
                self.parked.store(false, Ordering::Relaxed);
                return ask
                    .ask_question(session_id, &question_text(call), &options)
                    .await;
            }
            self.parked.store(true, Ordering::Relaxed);
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
        tool_name == ASK_HUMAN_TOOL && self.parked.load(Ordering::Relaxed)
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

    struct Chooser {
        answer: String,
    }

    #[async_trait]
    impl PermissionAsk for Chooser {
        async fn ask(
            &self,
            _session_id: &str,
            _program: &str,
            _args: &[String],
        ) -> Result<crate::permission::PermissionDecision, String> {
            Err("not a command".into())
        }
        async fn ask_question(
            &self,
            _session_id: &str,
            _question: &str,
            _options: &[String],
        ) -> Result<String, String> {
            Ok(self.answer.clone())
        }
    }

    #[tokio::test]
    async fn options_wait_on_the_client_and_do_not_park() {
        let runtime = wrap_with_client(
            Arc::new(Stub),
            true,
            Some((
                Arc::new(Chooser {
                    answer: "acp-bridge".into(),
                }),
                "s1".into(),
            )),
        );
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
        assert_eq!(out, "acp-bridge");
        assert!(
            !runtime.parks_for_human(ASK_HUMAN_TOOL),
            "a chooser answer is a tool result; the turn continues"
        );
    }

    #[tokio::test]
    async fn a_free_text_question_still_parks() {
        let runtime = wrap_with_client(
            Arc::new(Stub),
            true,
            Some((
                Arc::new(Chooser {
                    answer: "unused".into(),
                }),
                "s1".into(),
            )),
        );
        let out = runtime
            .invoke(&ToolInvocation::new(
                "1",
                ASK_HUMAN_TOOL,
                json!({ "question": "What next?" }),
            ))
            .await
            .unwrap();
        assert!(out.contains("What next?"), "{out}");
        assert!(runtime.parks_for_human(ASK_HUMAN_TOOL));
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
