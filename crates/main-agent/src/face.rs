//! Face-agent tool surface: human interfacer + built-in `delegate` (dispatcher bridge)
//! and privileged `create_agent` (Agents-shelf spawn).
//!
//! Optional extra MCP tools (from `"main-agent"` policy grants) can be layered on for power users;
//! the architecture intent is that those stay empty and work goes through `delegate`.
//!
//! # One execution engine (E4)
//!
//! `delegate` starts a hosted background session on the [`GoalSessionHub`] (domain `"dispatch"`)
//! and awaits its terminal result. It no longer owns a dispatcher/orchestrator pair — those live
//! only inside `liberado-dispatch-pack`. Delegated sessions run **without** `AskHuman` (D-e).
//!
//! # Privileged agent spawn
//!
//! `create_agent` opens a long-lived specialist **chat** on the Agents shelf via
//! `create_with_grant` (Reading B stamp). It is **not** GoalSessionHub / `delegate`. Privilege
//! gate A: only offered when the current session's profile is an agent-creator
//! ([`liberado_conversation_store::is_agent_creator_profile`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use liberado_common::CapabilitySet;
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use liberado_session::{
    DomainHint, GoalSessionHub, GoalSpec, SessionGrant, SessionOrigin, TerminalKind,
};
use serde_json::json;

#[path = "face/create_agent.rs"]
mod create_agent;
pub use create_agent::{
    AgentSpawner, CREATE_AGENT_TOOL_NAME, CreateAgentResult, create_agent_tool_def,
};
use create_agent::{CreateAgentArgs, parse_create_agent_args};

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
        deferral: &AtomicBool,
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
                    ..Default::default()
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

        // Gap 2: if the subagent deferred the action to the human AND already surfaced it
        // out-of-band (an interactive proposal/permission notification went out), record it so the
        // face turn drops the redundant chat reply — the notification is the sole communication.
        // OR into the flag: a face turn may `delegate` more than once, and any one deferral counts.
        let deferred = result
            .and_then(|r| r.diagnostics.get("deferred_to_human"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if deferred {
            deferral.store(true, Ordering::Relaxed);
        }

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

/// Tool runtime shown to the main (face) agent: optional extras + `delegate` when bridged +
/// privileged `create_agent` when a spawner is attached.
pub struct FaceRuntime {
    bridge: Option<Arc<DispatchBridge>>,
    /// Capability-scoped optional MCP tools the operator granted to `"main-agent"`.
    extras: Arc<dyn ToolRuntime>,
    /// Parent face-chat session id for dispatch journals.
    parent_conversation: Option<String>,
    /// Per-turn flag raised when a `delegate` came back deferred-and-notified-out-of-band, read by
    /// the session after the turn to drop the redundant chat reply (Gap 2). Shared with the caller;
    /// a fresh `false` per turn.
    turn_deferral: Arc<AtomicBool>,
    /// When `Some`, the face may call `create_agent`. Absent for non-creator profiles (gate A).
    agent_spawner: Option<Arc<dyn AgentSpawner>>,
}

impl FaceRuntime {
    pub fn new(
        bridge: Option<Arc<DispatchBridge>>,
        extras: Arc<dyn ToolRuntime>,
        parent_conversation: Option<String>,
        turn_deferral: Arc<AtomicBool>,
        agent_spawner: Option<Arc<dyn AgentSpawner>>,
    ) -> Self {
        Self {
            bridge,
            extras,
            parent_conversation,
            turn_deferral,
            agent_spawner,
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

    fn builtin_catalog(&self) -> Vec<ToolDef> {
        let mut tools = Vec::new();
        if self.bridge.is_some() {
            tools.push(Self::delegate_tool_def());
        }
        if self.agent_spawner.is_some() {
            tools.push(create_agent_tool_def());
        }
        tools
    }

    async fn invoke_delegate(&self, call: &ToolInvocation) -> Result<String, String> {
        let Some(bridge) = &self.bridge else {
            return Err("delegate is not available (no dispatcher attached)".into());
        };
        let goal = parse_delegate_goal(&call.arguments)?;
        bridge
            .delegate(
                goal.as_str(),
                self.parent_conversation.as_deref(),
                &self.turn_deferral,
            )
            .await
    }

    async fn invoke_create_agent(&self, call: &ToolInvocation) -> Result<String, String> {
        let Some(spawner) = &self.agent_spawner else {
            return Err(
                "create_agent is not available (this session is not an agent creator)".into(),
            );
        };
        let CreateAgentArgs { profile, title } = parse_create_agent_args(&call.arguments)?;
        Ok(spawner.spawn_agent(&profile, title).await?.to_json())
    }

    async fn try_invoke_builtin(&self, call: &ToolInvocation) -> Option<Result<String, String>> {
        if call.name == DELEGATE_TOOL_NAME {
            return Some(self.invoke_delegate(call).await);
        }
        if call.name == CREATE_AGENT_TOOL_NAME {
            return Some(self.invoke_create_agent(call).await);
        }
        None
    }
}

#[async_trait]
impl ToolRuntime for FaceRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        let mut tools = self.builtin_catalog();
        tools.extend(self.extras.catalog());
        tools
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if let Some(result) = self.try_invoke_builtin(call).await {
            return result;
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
#[path = "face/tests.rs"]
mod survivor_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_executor::ToolRuntime;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn parse_goal_merges_context() {
        let args = json!({"goal": "list tasks", "context": "inbox only"});
        let g = parse_delegate_goal(&args).unwrap();
        assert!(g.contains("list tasks"));
        assert!(g.contains("inbox only"));
    }

    struct NoExtras;
    #[async_trait]
    impl ToolRuntime for NoExtras {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _: &ToolInvocation) -> Result<String, String> {
            Err("no extras".into())
        }
    }

    struct StubSpawner;
    #[async_trait]
    impl AgentSpawner for StubSpawner {
        async fn spawn_agent(
            &self,
            profile: &str,
            title: Option<String>,
        ) -> Result<CreateAgentResult, String> {
            Ok(CreateAgentResult {
                conversation_id: "01TEST".into(),
                profile: profile.into(),
                title,
            })
        }
    }

    #[test]
    fn create_agent_absent_from_catalog_without_spawner() {
        let rt = FaceRuntime::new(
            None,
            Arc::new(NoExtras),
            None,
            Arc::new(AtomicBool::new(false)),
            None,
        );
        assert!(
            !rt.catalog()
                .iter()
                .any(|t| t.name == CREATE_AGENT_TOOL_NAME)
        );
    }

    #[test]
    fn create_agent_present_when_spawner_attached() {
        let rt = FaceRuntime::new(
            None,
            Arc::new(NoExtras),
            None,
            Arc::new(AtomicBool::new(false)),
            Some(Arc::new(StubSpawner)),
        );
        assert!(
            rt.catalog()
                .iter()
                .any(|t| t.name == CREATE_AGENT_TOOL_NAME)
        );
    }
}
