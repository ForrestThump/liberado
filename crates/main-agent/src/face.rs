//! Face-agent tool surface: human interfacer + built-in `delegate` (dispatcher bridge).
//!
//! Optional extra MCP tools (from `"main-agent"` policy grants) can be layered on for power users;
//! the architecture intent is that those stay empty and work goes through `delegate`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{
    CapabilityCatalog, CapabilitySet, PROPOSALS_DIR, SignedProposal, WriteClass,
};
use liberado_dispatcher::{DispatchRequest, Dispatcher};
use liberado_executor::ToolRuntime;
use liberado_orchestrator::{Disposition, Orchestrator};
use liberado_provider::{ToolDef, ToolInvocation};
use liberado_session::{
    BackgroundRun, DomainHint, GoalSpec, SessionGrant, SessionOrigin, SessionRecordStore,
    TerminalKind,
};
use serde_json::json;

/// Tool name the face agent calls to hand a goal to the dispatcher.
pub const DELEGATE_TOOL_NAME: &str = "delegate";

/// The `domain` recorded on a delegated subagent's session. Like a daemon reaction, it is run by the
/// dispatcher/orchestrator rather than by a domain pack — so it is not `coding` or `life`, and
/// joining one is read-only.
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

/// Shared dispatch/orchestrator bridge used by the face agent's `delegate` tool.
pub struct DispatchBridge {
    pub dispatcher: Arc<Dispatcher>,
    pub orchestrator: Arc<Orchestrator>,
    pub catalog: Arc<CapabilityCatalog>,
    /// Ceiling for classification + worker execution (policy component `"dispatcher"`).
    pub dispatcher_capabilities: CapabilitySet,
    pub zone_write_classes: Vec<(String, WriteClass)>,
    /// Vault `proposals/` directory — Propose dispositions are written under
    /// `proposals_dir/proposals/<id>.md` (same layout as chat/`RiskGatedToolRuntime`).
    pub proposals_dir: PathBuf,
    /// Where a delegation is recorded as a **background session** (S5′ step 5) — `None` leaves the
    /// old behavior exactly as it was.
    ///
    /// A subagent is the third unattended trigger, alongside cron and webhooks, and the one that was
    /// hardest to see: its work vanished into a dispatch journal, and the chat got back only a
    /// summary paragraph. Recording it as a session makes it a **child of the chat that spawned it**
    /// (`parent_session`), so "what did that actually do?" has an answer you can open.
    pub sessions: Option<Arc<dyn SessionRecordStore>>,
}

impl DispatchBridge {
    /// Run the dispatcher → orchestrator path and return a **compact report** for the face agent
    /// (never raw tool dumps).
    ///
    /// `parent_conversation` is the face chat session id (if any), written into the delegation
    /// journal under `.liberado/dispatches/<correlation_id>.jsonl` for ops/debug (not model context).
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
        let model = None; // model lives on the shared provider; logged in dispatch spans
        tracing::info!(
            %correlation_id,
            parent = parent_conversation.unwrap_or("-"),
            goal = %goal.chars().take(160).collect::<String>(),
            "face agent delegating to dispatcher"
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

        // The delegation becomes a session of its own — a *child* of the chat that asked for it.
        // Opened before dispatch, so a subagent that is still working is visible while it works,
        // rather than appearing only once it is over (or never, if it dies).
        let run = match &self.sessions {
            Some(store) => Some(
                BackgroundRun::open(
                    store.clone(),
                    delegated_goal(goal, &correlation_id, parent_conversation),
                    SessionGrant {
                        capabilities: self.dispatcher_capabilities.clone(),
                        profile: None,
                        overrides: serde_json::Value::Null,
                    },
                )
                .await,
            ),
            None => None,
        };

        let req = DispatchRequest {
            goal: goal.to_string(),
            catalog: self.catalog.descriptors(),
            capabilities: self.dispatcher_capabilities.clone(),
            reaction_depth: 0,
            zone_write_classes: self.zone_write_classes.clone(),
        };

        let decision = match self.dispatcher.dispatch(&req).await {
            Ok(d) => d,
            Err(e) => {
                let message = format!("dispatch failed: {e}");
                if let Some(run) = run {
                    run.finish(TerminalKind::Failed, message.clone()).await;
                }
                return Err(message);
            }
        };

        crate::dispatch_journal::append(
            &correlation_id,
            crate::dispatch_journal::decision_record(&decision, model),
        )
        .await;

        if let Some(run) = &run {
            run.progress(format!(
                "dispatched: {} (confidence {:.2}) — {}",
                decision.action.label(),
                decision.confidence,
                decision.rationale
            ))
            .await;
        }

        // ExecuteDirect also goes through the orchestrator worker path so the face agent
        // never receives tool schemas — only a report summary.
        let disposition = match self.orchestrator.run(decision, goal, &correlation_id).await {
            Ok(d) => d,
            Err(e) => {
                let message = format!("orchestration failed: {e}");
                if let Some(run) = run {
                    run.finish(TerminalKind::Failed, message.clone()).await;
                }
                return Err(message);
            }
        };

        let mut summary = format_disposition(&disposition, &self.proposals_dir).await;

        // Close the session on the *clean* summary, before the journal footer is appended below —
        // that footer is a hint for the face agent's next turn, not part of what the subagent did.
        if let Some(run) = run {
            let (terminal, note) = disposition.terminal_summary();
            run.finish(terminal, note).await;
        }

        let journal = crate::dispatch_journal::journal_display_path(&correlation_id);
        summary.push_str(&format!(
            "\n\n[dispatch journal: {journal} | id: {correlation_id}"
        ));
        if let Some(parent) = parent_conversation {
            summary.push_str(&format!(" | parent chat: {parent}"));
        }
        summary.push(']');

        crate::dispatch_journal::append(
            &correlation_id,
            crate::dispatch_journal::disposition_record(&summary, model),
        )
        .await;

        Ok(summary)
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

async fn format_disposition(d: &Disposition, proposals_dir: &Path) -> String {
    match d {
        Disposition::Clarify {
            questions,
            what_blocked,
        } => {
            let mut out = String::from(
                "NEEDS_CLARIFICATION: The system cannot proceed without more information from the human.\n",
            );
            out.push_str(&format!("Blocked by: {what_blocked:?}\n"));
            out.push_str("Ask the human:\n");
            for q in questions {
                out.push_str("- ");
                out.push_str(q);
                out.push('\n');
            }
            out
        }
        Disposition::Reported(report) => {
            format!("RESULT ({:?}):\n{}", report.outcome, report.summary.trim())
        }
        Disposition::Propose(proposal) => format_propose(proposal, proposals_dir).await,
    }
}

/// Persist a face-path proposal note, then describe it for the face agent (dogfood D2).
async fn format_propose(proposal: &SignedProposal, proposals_dir: &Path) -> String {
    match write_face_proposal(proposal, proposals_dir).await {
        Ok(path) => format!(
            "PROPOSAL: A high-consequence action needs human approval before it runs.\n\
             Proposal id: {}\n\
             Draft saved at: {}\n\
             Tell the human to review and approve (or reject) that note in the vault proposals folder.",
            proposal.id,
            path.display()
        ),
        Err(e) => {
            tracing::error!(
                error = %e,
                proposal_id = %proposal.id,
                "face delegate failed to write proposal note"
            );
            format!(
                "PROPOSAL_FAILED: The system wanted human approval (proposal id {}) but could not \
                 save the draft note ({e}). Tell the human honestly — there is nothing to approve \
                 on disk until this is retried successfully.",
                proposal.id
            )
        }
    }
}

async fn write_face_proposal(
    proposal: &SignedProposal,
    proposals_dir: &Path,
) -> std::io::Result<PathBuf> {
    let proposals_subdir = proposals_dir.join(PROPOSALS_DIR);
    let proposal_path = proposals_subdir.join(format!("{}.md", proposal.id));
    tokio::fs::create_dir_all(&proposals_subdir).await?;
    tokio::fs::write(&proposal_path, proposal.to_note()).await?;
    Ok(proposal_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::{BlockReason, Outcome, Proposal, ProposalSigner, ProposedAction, Report};

    #[tokio::test]
    async fn format_clarify_lists_questions() {
        let d = Disposition::Clarify {
            questions: vec!["which folder?".into()],
            what_blocked: BlockReason::Ambiguous,
        };
        let s = format_disposition(&d, Path::new(".")).await;
        assert!(s.contains("NEEDS_CLARIFICATION"));
        assert!(s.contains("which folder?"));
    }

    #[tokio::test]
    async fn format_report_includes_summary() {
        let d = Disposition::Reported(Report {
            outcome: Outcome::Succeeded,
            summary: "done".into(),
            artifacts: vec![],
            new_high_signal_facts: vec![],
            follow_up: None,
        });
        let s = format_disposition(&d, Path::new(".")).await;
        assert!(s.contains("RESULT"));
        assert!(s.contains("done"));
    }

    #[test]
    fn parse_goal_merges_context() {
        let args = json!({"goal": "list tasks", "context": "inbox only"});
        let g = parse_delegate_goal(&args).unwrap();
        assert!(g.contains("list tasks"));
        assert!(g.contains("inbox only"));
    }

    #[tokio::test]
    async fn format_propose_writes_note_and_returns_path() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_root = dir.path().join("proposals");
        let signer = ProposalSigner::random();
        let pending = Proposal::pending(
            "face-prop-1",
            "face-prop-1",
            "test",
            ProposedAction::Subagent {
                goal: "list tasks".into(),
                capabilities: CapabilitySet::empty(),
                allowed_mcps: vec!["turbovault".into()],
                success_criteria: vec![],
            },
            "test rationale",
        );
        let signed = signer.sign(pending);
        let d = Disposition::Propose(signed);
        let s = format_disposition(&d, &proposals_root).await;
        assert!(s.contains("PROPOSAL:"), "{s}");
        assert!(s.contains("Draft saved at:"), "{s}");
        assert!(s.contains("face-prop-1"), "{s}");
        let note = proposals_root.join(PROPOSALS_DIR).join("face-prop-1.md");
        assert!(note.is_file(), "expected proposal at {}", note.display());
    }
}
