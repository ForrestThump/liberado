//! Automation Editor chats and the schedule-edit proposal they file.
//!
//! Each open creates a new chat. The profile is `automation-editor` and the
//! header flag `automation_editor` selects the schedule-tool face. That name
//! resolves as a built-in when topology has no matching row. Agent Creator
//! does not need this: it runs as `operator`, which deployments already list.
//! `is_agent_creator_profile` stays false for the editor name, so the session
//! does not gain `create_agent`. Writes file a permission card and return.
//! They do not edit `topology.toml`.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{
    ApprovalOrigin, Capability, PROPOSALS_DIR, Proposal, ProposedAction, ToolCall,
};
use liberado_conversation_store::{Author, NewNode, SurfaceMode, Ulid};
use liberado_provider::Message;
use serde_json::Value;

use super::ChatSessions;
use super::surface_mode::ConversationCreate;
use crate::face::{ScheduleEditorOps, is_schedule_write_tool};

/// Profile the editor runs under. Not an agent-creator profile.
pub const AUTOMATION_EDITOR_PROFILE: &str = "automation-editor";

/// First assistant message. Written by the store, not by a model turn.
pub const AUTOMATION_EDITOR_OPENER: &str = "What automation changes do you want to make?";

/// What the model sees when a card is filed and the file is still unchanged.
pub const SCHEDULE_PROPOSAL_ACK: &str = "\
Filed an approval card in this chat. topology.toml was not changed. \
Approve applies this one edit. Deny writes nothing.";

const AUTOMATION_EDITOR_SYSTEM_PROMPT: &str = "\
You are Liberado's Automation Editor. This chat edits schedules in topology.toml.

The first assistant message is already in this chat. Do not repeat it. \
When this prompt names a target automation, that context is already given. \
Do not ask which automation the human opened. When it names none, they are \
starting a new automation.

Tools:
- list_schedules reads the file. It does not need approval.
- create_schedule, update_schedule, and delete_schedule file an approval card. \
They do not change the file. Wait for the human to tap Approve or Deny.
- Approve applies that one edit. Deny writes nothing.
- After a successful Approve, tell the human the edit is saved and takes effect \
after the service restarts. Cron and the schedules list keep the previous copy until then.

You do not have delegate or create_agent. Do not claim a write happened before Approve. \
Prefer the structured form for a weekday and a clock: days (mon, tue, wed, thu, fri, sat, sun), \
at (\"20:00\"), and timezone (an IANA name). Omit timezone to use the topology timezone. \
That form is evaluated in the timezone, so the clock stays put across DST. \
Use exactly one of the structured form or cron_expr. When you switch forms, clear the other \
(cron_expr, or days and at). \
cron_expr is the escape hatch: seconds first, and UTC unless timezone is also set. \
When timezone is set, cron_expr fields are civil time in that zone. \
Do not present the UTC hour as the local time. \
Judge timing only from next_fires and the code-generated description; never from names, comments or labels; compare them to what the user asked and say so if they disagree. \
Quote times in Central time (America/Chicago) when that is the timezone. \
Be concise.";

/// The automation an Edit click opened. A new automation has no target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorTarget {
    pub name: String,
    pub enabled: bool,
    pub schedule_text: String,
    pub cron_expr: String,
    pub days: Vec<String>,
    pub at: String,
    pub timezone: String,
    pub description: String,
}

/// Hidden prompt text for one Edit click. Not a chat turn.
pub fn editor_target_context(target: &EditorTarget) -> String {
    format!(
        "Target automation (already given; do not ask which one):\n\
         Name (id): {name}.\n\
         State: {state}.\n\
         Schedule: {when}.\n\
         {form}\
         Cron expression: {cron}.\n\
         Timezone: {zone}.\n\
         What it does: {description}.",
        name = target.name.trim(),
        state = enabled_word(target.enabled),
        when = display_schedule(target),
        form = form_lines(target),
        cron = blank_as(target.cron_expr.trim(), "not set"),
        zone = blank_as(target.timezone.trim(), "not set"),
        description = blank_as(target.description.trim(), "No description is stored."),
    )
}

/// System prompt for one editor chat. Edit appends [`editor_target_context`].
pub fn editor_system_prompt(target: Option<&EditorTarget>) -> String {
    match target {
        Some(target) => format!(
            "{AUTOMATION_EDITOR_SYSTEM_PROMPT}\n\n{}",
            editor_target_context(target)
        ),
        None => AUTOMATION_EDITOR_SYSTEM_PROMPT.to_string(),
    }
}

/// Sidebar title. Edit names the row. New does not.
pub fn editor_chat_title(target: Option<&EditorTarget>) -> String {
    match target
        .map(|row| row.name.trim())
        .filter(|name| !name.is_empty())
    {
        Some(name) => format!("Edit {name}"),
        None => "New automation".to_string(),
    }
}

fn enabled_word(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn form_lines(target: &EditorTarget) -> String {
    if target.days.is_empty() && target.at.trim().is_empty() {
        return String::new();
    }
    let days = if target.days.is_empty() {
        "not set".to_string()
    } else {
        target.days.join(", ")
    };
    format!(
        "Days: {days}.\nAt: {}.\n",
        blank_as(target.at.trim(), "not set")
    )
}

fn display_schedule(target: &EditorTarget) -> String {
    let text = target.schedule_text.trim();
    if text.is_empty() {
        blank_as(target.cron_expr.trim(), "not set")
    } else {
        text.to_string()
    }
}

fn proposal_ack(sessions: &ChatSessions, tool: &str, args: &Value) -> String {
    let Some(facts) = &sessions.schedule_facts else {
        return SCHEDULE_PROPOSAL_ACK.to_string();
    };
    let extra = facts(tool, args);
    if extra.trim().is_empty() {
        SCHEDULE_PROPOSAL_ACK.to_string()
    } else {
        format!("{SCHEDULE_PROPOSAL_ACK}\n{extra}")
    }
}

fn blank_as(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

/// Re-read of `[[schedules]]` for `list_schedules`. The server wires the file reader.
pub(super) type ScheduleList = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

/// Code-computed timing for a schedule tool result. The server wires this.
pub(super) type ScheduleFacts = Arc<dyn Fn(&str, &Value) -> String + Send + Sync>;

impl ChatSessions {
    /// Install the file reader `list_schedules` calls. Boot wires this once.
    pub fn with_schedule_list(mut self, list: ScheduleList) -> Self {
        self.schedule_list = Some(list);
        self
    }

    /// Install the timing text `propose_edit` appends. Boot wires this once.
    pub fn with_schedule_facts(mut self, facts: ScheduleFacts) -> Self {
        self.schedule_facts = Some(facts);
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

    /// Open a new Automation Editor chat.
    ///
    /// Every call creates a conversation. `target` is hidden prompt context for
    /// an Edit click. `None` is a new automation. The opener is one assistant
    /// node written here, not a model turn.
    pub async fn open_automation_editor(
        &self,
        target: Option<EditorTarget>,
    ) -> Result<Ulid, String> {
        let resolver = self.profile_resolver.as_ref().ok_or_else(|| {
            "automation editor is not configured (no profile resolver wired)".to_string()
        })?;
        let grant = resolver(AUTOMATION_EDITOR_PROFILE)?;
        let id = self
            .create_stamped(ConversationCreate {
                title: Some(editor_chat_title(target.as_ref())),
                ephemeral: false,
                visibility: liberado_session::Visibility::Foreground,
                grant,
                explicit_surface: Some(SurfaceMode::Chat),
                agent_creator: false,
                automation_editor: true,
                system_prompt: Some(editor_system_prompt(target.as_ref())),
            })
            .await
            .map_err(|e| e.to_string())?;
        self.seed_editor_opener(id).await?;
        Ok(id)
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
        write_schedule_proposal(self, session_id, tool, args.clone(), rationale).await?;
        Ok(proposal_ack(self, tool, &args))
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
