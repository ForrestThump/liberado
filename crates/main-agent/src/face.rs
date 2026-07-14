//! Face-agent tool surface: human interfacer + built-in `delegate` (dispatcher bridge).
//!
//! Optional extra MCP tools (from `"main-agent"` policy grants) can be layered on for power users;
//! the architecture intent is that those stay empty and work goes through `delegate`.
//!
//! # One execution engine (E4)
//!
//! `delegate` starts a hosted background session on the [`GoalSessionHub`] (domain `"dispatch"`)
//! and awaits its terminal result. It no longer owns a dispatcher/orchestrator pair — those live
//! only inside `liberado-dispatch-pack`. Delegated sessions run **without** `AskHuman` (D-e).

use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::CapabilitySet;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use liberado_session::{
    DomainHint, GoalSessionHub, GoalSpec, SessionGrant, SessionOrigin, TerminalKind,
};
use serde_json::json;

/// Tool name the face agent calls to hand a goal to the dispatcher.
pub const DELEGATE_TOOL_NAME: &str = "delegate";

/// The `domain` of a delegated subagent's session — the dispatch pack.
const DELEGATE_DOMAIN: &str = "dispatch";

/// The goal recorded for a delegation. Unlike a cron, a subagent *does* have a parent conversation:
/// the chat whose face agent called `delegate`. Carrying it makes the session a real child edge in
/// the store, so the tree from chat → subagent is walkable.
fn delegated_goal(goal: &str, correlation_id: &str, parent_conversation: Option<&str>) -> GoalSpec {
    GoalSpec {
        id: None,
        description: goal.to_string(),
        success_criteria: Vec::new(),
        domain: DomainHint::from(DELEGATE_DOMAIN),
        max_turns: 0,
        max_idle_secs: None,
        origin: Some(SessionOrigin {
            conversation_id: parent_conversation.map(str::to_string),
            correlation_id: Some(correlation_id.to_string()),
        }),
        profile: None,
        payload: json!({ "source": "delegate" }),
    }
}

/// Shared bridge used by the face agent's `delegate` tool — starts a hub session and awaits it.
pub struct DispatchBridge {
    pub hub: Arc<GoalSessionHub>,
    /// Ceiling for the delegated session (policy component `"dispatcher"`). No `AskHuman` (D-e).
    pub dispatcher_capabilities: CapabilitySet,
}

impl DispatchBridge {
    /// Start a background dispatch session and return a **compact report** for the face agent
    /// (never raw tool dumps). Blocks until the session is terminal — same as the old inline
    /// `orchestrator.run` path, so the chat turn is not newly blocking.
    ///
    /// `parent_conversation` is the face chat session id (if any), written into the session origin
    /// and the dispatch journal under `.liberado/dispatches/<correlation_id>.jsonl`.
    pub async fn delegate(
        &self,
        goal: &str,
        parent_conversation: Option<&str>,
    ) -> Result<String, String> {
        let goal = goal.trim();
        if goal.is_empty() {
            return Err("delegate requires a non-empty `goal`".into());
        }

        let correlation_id = format!("chat-delegate-{}", ulid::Ulid::new());
        let model = None;
        tracing::info!(
            %correlation_id,
            parent = parent_conversation.unwrap_or("-"),
            goal = %goal.chars().take(160).collect::<String>(),
            "face agent delegating via goal session hub"
        );

        crate::dispatch_journal::append(
            &correlation_id,
            crate::dispatch_journal::start_record(
                &correlation_id,
                parent_conversation,
                goal,
                model,
            ),
        )
        .await;

        // D-e: delegated sessions run without AskHuman. Strip it even if the dispatcher grant
        // happens to include it (a misconfigured policy must not turn a chat-turn into a multi-hour
        // wait on a human the face agent cannot relay mid-turn).
        let mut capabilities = self.dispatcher_capabilities.clone();
        capabilities
            .capabilities
            .retain(|c| !matches!(c, liberado_common::Capability::AskHuman));

        let session_id = self
            .hub
            .start_background(
                delegated_goal(goal, &correlation_id, parent_conversation),
                SessionGrant {
                    capabilities,
                    profile: None,
                    overrides: serde_json::Value::Null,
                },
            )
            .await
            .map_err(|e| format!("failed to start delegated session: {e}"))?;

        let snap = self
            .hub
            .await_terminal(&session_id)
            .await
            .map_err(|e| format!("delegated session failed to finish: {e}"))?;

        let result = snap.session.result.as_ref();
        let summary = result
            .map(|r| r.summary.clone())
            .unwrap_or_else(|| "delegated session finished with no summary".into());
        let terminal = result.map(|r| r.terminal).unwrap_or(TerminalKind::Failed);

        let mut report = match terminal {
            TerminalKind::Succeeded => format!("RESULT (Succeeded):\n{}", summary.trim()),
            TerminalKind::Failed => format!("RESULT (Failed):\n{}", summary.trim()),
            TerminalKind::Cancelled => format!("RESULT (Cancelled):\n{}", summary.trim()),
            TerminalKind::BudgetExhausted => {
                format!("RESULT (BudgetExhausted):\n{}", summary.trim())
            }
        };
        report.push_str(&format!("\n[session: {session_id}]"));

        let journal = crate::dispatch_journal::journal_display_path(&correlation_id);
        report.push_str(&format!(
            "\n\n[dispatch journal: {journal} | id: {correlation_id}"
        ));
        if let Some(parent) = parent_conversation {
            report.push_str(&format!(" | parent chat: {parent}"));
        }
        report.push(']');

        crate::dispatch_journal::append(
            &correlation_id,
            crate::dispatch_journal::disposition_record(&report, model),
        )
        .await;

        Ok(report)
    }
}

/// Tool runtime shown to the main (face) agent: optional extras + always `delegate` when bridged.
pub struct FaceRuntime {
    bridge: Option<Arc<DispatchBridge>>,
    /// Capability-scoped optional MCP tools the operator granted to `"main-agent"`.
    extras: Arc<dyn ToolRuntime>,
    /// Parent face-chat session id for dispatch journals.
    parent_conversation: Option<String>,
}

impl FaceRuntime {
    pub fn new(
        bridge: Option<Arc<DispatchBridge>>,
        extras: Arc<dyn ToolRuntime>,
        parent_conversation: Option<String>,
    ) -> Self {
        Self {
            bridge,
            extras,
            parent_conversation,
        }
    }

    pub fn delegate_tool_def() -> ToolDef {
        ToolDef::new(
            DELEGATE_TOOL_NAME,
            "Hand a fully-understood goal to Liberado's dispatcher (which routes it to domain packs, \
             tools, and subagents). Use this whenever the human needs real-world action, lookup, or \
             multi-step work — you do NOT have those tools yourself. The system has broad \
             capabilities (vault, tasks, external services, and more); if something is missing, it \
             may propose creating it. Pass a clear, self-contained goal. Returns a short result, \
             clarifying questions for you to ask the human, or a proposal path for human approval.",
            json!({
                "type": "object",
                "properties": {
                    "goal": {
                        "type": "string",
                        "description": "What should be accomplished. Include concrete details the human already provided."
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional extra context from the conversation that helps execution."
                    }
                },
                "required": ["goal"]
            }),
        )
    }
}

#[async_trait]
impl ToolRuntime for FaceRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        let mut tools = Vec::new();
        if self.bridge.is_some() {
            tools.push(Self::delegate_tool_def());
        }
        tools.extend(self.extras.catalog());
        tools
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if call.name == DELEGATE_TOOL_NAME {
            let Some(bridge) = &self.bridge else {
                return Err("delegate is not available (no dispatcher attached)".into());
            };
            let goal = parse_delegate_goal(&call.arguments)?;
            return bridge
                .delegate(goal.as_str(), self.parent_conversation.as_deref())
                .await;
        }
        self.extras.invoke(call).await
    }
}

fn parse_delegate_goal(arguments: &serde_json::Value) -> Result<String, String> {
    let goal = arguments
        .get("goal")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let context = arguments
        .get("context")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if goal.is_empty() {
        return Err("delegate requires a non-empty `goal` string".into());
    }
    if context.is_empty() {
        Ok(goal.to_string())
    } else {
        Ok(format!("{goal}\n\nContext:\n{context}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_goal_merges_context() {
        let args = json!({"goal": "list tasks", "context": "inbox only"});
        let g = parse_delegate_goal(&args).unwrap();
        assert!(g.contains("list tasks"));
        assert!(g.contains("inbox only"));
    }
}
