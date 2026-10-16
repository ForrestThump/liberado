//! Singleton Automation Editor session and the schedule-edit proposal it files.
//!
//! Copy of the Agent Creator open path. The profile is `automation-editor`.
//! `is_agent_creator_profile` stays false for that name, so the session does not
//! gain `create_agent`. Writes file a permission card and return. They do not
//! edit `topology.toml`.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{
    ApprovalOrigin, Capability, PROPOSALS_DIR, Proposal, ProposedAction, ToolCall,
};
use liberado_conversation_store::{Author, ConversationHeader, NewNode, SurfaceMode, Ulid};
use liberado_provider::Message;
use serde_json::Value;

use super::ChatSessions;
use super::agent_spawn::{agent_shelf_lock, pick_oldest};
use super::surface_mode::ConversationCreate;
use crate::face::{ScheduleEditorOps, is_schedule_write_tool};

/// Profile the editor runs under. Not an agent-creator profile.
pub const AUTOMATION_EDITOR_PROFILE: &str = "automation-editor";

/// Sidebar title. Display only. The singleton marker is the header flag.
pub const AUTOMATION_EDITOR_TITLE: &str = "Automation Editor";

/// First assistant message. Written by the store, not by a model turn.
pub const AUTOMATION_EDITOR_OPENER: &str = "Which schedule should I list or change?";

/// What the model sees when a card is filed and the file is still unchanged.
pub const SCHEDULE_PROPOSAL_ACK: &str = "\
Filed an approval card in this chat. topology.toml was not changed. \
Approve applies this one edit. Deny writes nothing.";

const AUTOMATION_EDITOR_SYSTEM_PROMPT: &str = "\
You are Liberado's Automation Editor. This chat edits cron schedules in topology.toml.

Tools:
- list_schedules reads the file. It does not need approval.
- create_schedule, update_schedule, and delete_schedule file an approval card. \
They do not change the file. Wait for the human to tap Approve or Deny.
- Approve applies that one edit. Deny writes nothing.
- After a successful Approve, the reply says the file is on disk. Cron and the \
schedules list still show the boot copy until the process is recreated.

You do not have delegate or create_agent. Do not claim a write happened before Approve. \
Be concise.";

/// Re-read of `[[schedules]]` for `list_schedules`. The server wires the file reader.
pub(super) type ScheduleList = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

impl ChatSessions {
    /// Install the file reader `list_schedules` calls. Boot wires this once.
    pub fn with_schedule_list(mut self, list: ScheduleList) -> Self {
        self.schedule_list = Some(list);
        self
    }

    /// Ops handle for [`crate::face::EditorRuntime`]. A missing self-handle
    /// still builds a runtime; calls then fail closed.
    pub(super) fn schedule_editor_ops(&self) -> Arc<dyn ScheduleEditorOps> {
        self.self_handle
            .get()
            .and_then(std::sync::Weak::upgrade)
            .map(|arc| arc as Arc<dyn ScheduleEditorOps>)
            .unwrap_or_else(unwired_editor)
    }

    /// Find or create the singleton Automation Editor.
    ///
    /// Returns `(conversation_id, created)`. A second open returns the same id.
    /// The opener is appended only while every node is still `Author::System`.
    pub async fn open_automation_editor(&self) -> Result<(Ulid, bool), String> {
        let _guard = agent_shelf_lock().lock().await;
        self.open_automation_editor_locked().await
    }

    async fn open_automation_editor_locked(&self) -> Result<(Ulid, bool), String> {
        if let Some(existing) = self.find_automation_editor().await? {
            self.seed_editor_opener(existing.id).await?;
            return Ok((existing.id, false));
        }
        let resolver = self.profile_resolver.as_ref().ok_or_else(|| {
            "automation editor is not configured (no profile resolver wired)".to_string()
        })?;
        let grant = resolver(AUTOMATION_EDITOR_PROFILE)?;
        let id = self
            .create_stamped(ConversationCreate {
                title: Some(AUTOMATION_EDITOR_TITLE.to_owned()),
                ephemeral: false,
                visibility: liberado_session::Visibility::Foreground,
                grant,
                explicit_surface: Some(SurfaceMode::Chat),
                agent_creator: false,
                automation_editor: true,
                system_prompt: Some(AUTOMATION_EDITOR_SYSTEM_PROMPT.to_owned()),
            })
            .await
            .map_err(|e| e.to_string())?;
        self.seed_editor_opener(id).await?;
        Ok((id, true))
    }

    async fn find_automation_editor(&self) -> Result<Option<ConversationHeader>, String> {
        let headers = self.store.list().await.map_err(|e| e.to_string())?;
        Ok(pick_oldest(headers.iter().filter(|h| h.automation_editor)).cloned())
    }

    async fn seed_editor_opener(&self, id: Ulid) -> Result<(), String> {
        let nodes = self
            .store
            .leaf_path(id, None)
            .await
            .map_err(|e| e.to_string())?;
        if nodes.iter().any(|n| !matches!(n.author, Author::System)) {
            return Ok(());
        }
        let parent = nodes.last().map(|n| n.id);
        self.store
            .append(
                id,
                NewNode {
                    parent_id: parent,
                    author: Author::Assistant,
                    message: Message::assistant(AUTOMATION_EDITOR_OPENER),
                    model: None,
                    reasoning: None,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[async_trait]
impl ScheduleEditorOps for ChatSessions {
    async fn list_schedules(&self) -> Result<String, String> {
        match &self.schedule_list {
            Some(list) => list(),
            None => Err("schedule list is not configured".into()),
        }
    }

    async fn propose_edit(
        &self,
        session_id: &str,
        tool: &str,
        args: Value,
        rationale: &str,
    ) -> Result<String, String> {
        if !is_schedule_write_tool(tool) {
            return Err(format!("unknown schedule tool `{tool}`"));
        }
        write_schedule_proposal(self, session_id, tool, args, rationale).await?;
        Ok(SCHEDULE_PROPOSAL_ACK.to_string())
    }
}

async fn write_schedule_proposal(
    sessions: &ChatSessions,
    session_id: &str,
    tool: &str,
    args: Value,
    rationale: &str,
) -> Result<(), String> {
    if sessions.proposals_dir.as_os_str().is_empty() {
        return Err(
            "schedule edits are not configured (no proposals directory). The file was not changed."
                .into(),
        );
    }
    let proposal_id = format!("sched-{}", ulid::Ulid::new());
    let signed = sessions.signer.sign(schedule_proposal(
        &proposal_id,
        session_id,
        tool,
        args,
        rationale,
    ));
    let dir = sessions.proposals_dir.join(PROPOSALS_DIR);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("could not create proposals directory: {e}"))?;
    let path = dir.join(format!("{proposal_id}.md"));
    tokio::fs::write(&path, signed.to_note())
        .await
        .map_err(|e| format!("could not save the approval card: {e}"))?;
    Ok(())
}

fn schedule_proposal(
    id: &str,
    session_id: &str,
    tool: &str,
    args: Value,
    rationale: &str,
) -> Proposal {
    let mut proposal = Proposal::pending(
        id,
        id,
        "automation-editor",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: tool.into(),
            args,
        }]),
        rationale,
    )
    .with_requested_grant(Capability::ExecuteTool(format!("automation-editor:{tool}")));
    proposal.session_id = Some(session_id.to_string());
    proposal.origin = Some(ApprovalOrigin::Web);
    proposal
}

struct UnwiredEditor;

#[async_trait]
impl ScheduleEditorOps for UnwiredEditor {
    async fn list_schedules(&self) -> Result<String, String> {
        Err("automation editor is not configured".into())
    }

    async fn propose_edit(&self, _: &str, _: &str, _: Value, _: &str) -> Result<String, String> {
        Err("automation editor is not configured. The file was not changed.".into())
    }
}

/// Used when `install_self_handle` was not called. Keeps the catalog buildable.
pub(super) fn unwired_editor() -> Arc<dyn ScheduleEditorOps> {
    Arc::new(UnwiredEditor)
}

#[cfg(test)]
#[path = "automation_editor_tests.rs"]
mod tests;
