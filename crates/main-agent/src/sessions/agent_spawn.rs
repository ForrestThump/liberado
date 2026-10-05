//! Privileged Agents-shelf create path (face `create_agent` + shared resolver).
//!
//! Kept off `sessions.rs` for module-health. The face tool requests create; this
//! module resolves the named profile (fail closed). A new identity calls
//! `create_with_grant` so Reading B stamps `Agent`. The same identity returns the
//! existing row. Not GoalSessionHub / `delegate`.
//!
//! `create_agent` is find-or-create: one session per [`agent_identity`]. The
//! Agents shelf + button opens [`ChatSessions::open_agent_creator`], a separate
//! singleton marked `agent_creator` (not an identity match).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use liberado_common::CapabilitySet;
use liberado_conversation_store::{Author, ConversationHeader, NewNode, SurfaceMode, Ulid};
use liberado_executor::ToolRuntime;
use liberado_provider::Message;
use liberado_session::SessionGrant;

use super::ChatSessions;
use super::surface_mode::ConversationCreate;
use crate::face::{AgentSpawner, CreateAgentResult, FaceRuntime};

/// Profile the Agent Creator session runs under so privilege gate A offers `create_agent`.
pub const AGENT_CREATOR_PROFILE: &str = "operator";

/// Sidebar title of the Agent Creator session. Display only — the singleton
/// marker is `ConversationHeader::agent_creator`, so a rename does not mint
/// another creator.
pub const AGENT_CREATOR_TITLE: &str = "Agent Creator";

/// First assistant message. Written by the store, not by a model turn.
pub const AGENT_CREATOR_OPENER: &str = "What type of agent do you want to make?";

/// Root system text for the creator session only. Replaces the shared face
/// prompt on this conversation so "delegate only" does not fight `create_agent`.
/// The WebUI hides system nodes; the human sees [`AGENT_CREATOR_OPENER`].
const AGENT_CREATOR_SYSTEM_PROMPT: &str = "\
You are Liberado's Agent Creator. This long-running chat makes specialist agents \
that land on the Agents shelf.

When the human describes an agent:
1. If the job is unclear, ask what it should do.
2. Choose an existing agent-eligible profile. That profile is the tool set. You \
cannot pass a custom capability list. Eligible names are the deployment's \
`[chat] agent_profiles` (default: coding, life, researcher, operator).
3. Choose a display name. That name is the `title` and the shelf row.
4. Call `create_agent` with that profile and title. The same profile and title \
reuse the existing session instead of creating a second one.
5. Tell the human the agent name and that it is on the Agents shelf.

Do not use `delegate` to create an agent. `create_agent` is the create path. \
Be concise.";

/// One Agents-shelf session per specialist.
///
/// Identity is `(profile, title)` after trim, compared case-sensitively.
/// A missing or blank title uses the profile name, so `create_agent("coding")`
/// and `create_agent("coding", title: "coding")` are the same agent. Two titles
/// under one profile are two agents (same hat, different shelf name).
///
/// The stored title is the identity. Renaming the row renames the agent:
/// a later call with the old title does not find it.
///
/// The Agent Creator session is excluded even when profile and title match.
pub(crate) fn agent_identity(profile: &str, title: Option<&str>) -> Option<(String, String)> {
    let profile = profile.trim();
    if profile.is_empty() {
        return None;
    }
    let title = title
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| profile.to_owned());
    Some((profile.to_owned(), title))
}

fn header_matches_agent(header: &ConversationHeader, profile: &str, title: &str) -> bool {
    !header.agent_creator
        && header.surface_mode == SurfaceMode::Agent
        && header.grant.profile.as_deref() == Some(profile)
        && header.title.as_deref() == Some(title)
}

fn pick_oldest<'a>(
    headers: impl Iterator<Item = &'a ConversationHeader>,
) -> Option<&'a ConversationHeader> {
    headers.min_by(|a, b| (&a.created_at, a.id).cmp(&(&b.created_at, b.id)))
}

fn agent_shelf_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Resolves a chat profile name → [`SessionGrant`]. Wired from the daemon's
/// `Config::resolve_session_profile` at boot (fail closed on unknown/disabled).
pub type ProfileGrantResolver = Arc<dyn Fn(&str) -> Result<SessionGrant, String> + Send + Sync>;

impl ChatSessions {
    /// Attach the profile→grant resolver used by `create_agent` (and any other
    /// in-process create that must fail closed on an unknown name).
    pub fn with_profile_resolver(mut self, resolver: ProfileGrantResolver) -> Self {
        self.profile_resolver = Some(resolver);
        self
    }

    /// Remember `Arc<Self>` so face tools can call back into create without a
    /// permanent cycle (stored as [`Weak`]). Call once after `Arc::new`.
    pub fn install_self_handle(self: &Arc<Self>) {
        let _ = self.self_handle.set(Arc::downgrade(self));
    }

    /// Upgrade the boot-time weak handle when the current session may spawn agents.
    pub(super) fn agent_spawner_for_profile(
        &self,
        profile: Option<&str>,
    ) -> Option<Arc<dyn AgentSpawner>> {
        if !liberado_conversation_store::is_agent_creator_profile(profile) {
            return None;
        }
        self.self_handle
            .get()
            .and_then(Weak::upgrade)
            .map(|arc| arc as Arc<dyn AgentSpawner>)
    }

    /// Create or reuse a long-lived Agent-shelf chat under a named agent-eligible profile.
    ///
    /// Agent-eligibility is checked against `self.agent_profiles` — the deployment's
    /// `[chat] agent_profiles` set, threaded through [`with_agent_profiles`](Self::with_agent_profiles)
    /// at boot. Tests that don't wire a deployment set get the conservative default.
    /// Grant = only what the resolver returns for `profile`. Rejects profiles not in
    /// the deployment's set and a missing resolver. Does not start a turn.
    ///
    /// Identity is [`agent_identity`]: the same profile and title return the existing
    /// conversation (`reused: true`) instead of calling `create_with_grant` again.
    pub async fn create_agent_chat(
        &self,
        profile: &str,
        title: Option<String>,
    ) -> Result<CreateAgentResult, String> {
        let _guard = agent_shelf_lock().lock().await;
        self.create_agent_chat_locked(profile, title).await
    }

    /// Find or create the singleton Agent Creator session.
    ///
    /// Returns `(conversation_id, created)`. `created` is false when the flagged
    /// row already existed. The canned opener is appended only while the
    /// transcript is still system-only, so a second open does not duplicate it
    /// and a later compaction does not re-seed it into the middle of the chat.
    pub async fn open_agent_creator(&self) -> Result<(Ulid, bool), String> {
        let _guard = agent_shelf_lock().lock().await;
        self.open_agent_creator_locked().await
    }

    async fn create_agent_chat_locked(
        &self,
        profile: &str,
        title: Option<String>,
    ) -> Result<CreateAgentResult, String> {
        let (profile, title) = agent_identity(profile, title.as_deref())
            .ok_or_else(|| "create_agent requires a non-empty profile".to_string())?;
        if !self.agent_profiles.is_agent(&profile) {
            return Err(format!(
                "profile `{profile}` is not in the deployment's [chat] agent_profiles set"
            ));
        }
        if let Some(existing) = self.find_agent_session(&profile, &title).await? {
            return Ok(CreateAgentResult {
                conversation_id: existing.id.to_string(),
                profile,
                title: existing.title.clone(),
                reused: true,
            });
        }
        let resolver = self.profile_resolver.as_ref().ok_or_else(|| {
            "create_agent is not configured (no profile resolver wired)".to_string()
        })?;
        let grant = resolver(&profile)?;
        let id = self
            .create_with_grant(Some(title.clone()), grant)
            .await
            .map_err(|e| e.to_string())?;
        Ok(CreateAgentResult {
            conversation_id: id.to_string(),
            profile,
            title: Some(title),
            reused: false,
        })
    }

    async fn open_agent_creator_locked(&self) -> Result<(Ulid, bool), String> {
        if let Some(existing) = self.find_agent_creator().await? {
            self.seed_opener_if_blank(existing.id).await?;
            return Ok((existing.id, false));
        }
        let resolver = self.profile_resolver.as_ref().ok_or_else(|| {
            "agent creator is not configured (no profile resolver wired)".to_string()
        })?;
        let grant = resolver(AGENT_CREATOR_PROFILE)?;
        let id = self
            .create_stamped(ConversationCreate {
                title: Some(AGENT_CREATOR_TITLE.to_owned()),
                ephemeral: false,
                visibility: liberado_session::Visibility::Foreground,
                grant,
                explicit_surface: Some(SurfaceMode::Agent),
                agent_creator: true,
                system_prompt: Some(AGENT_CREATOR_SYSTEM_PROMPT.to_owned()),
            })
            .await
            .map_err(|e| e.to_string())?;
        self.seed_opener_if_blank(id).await?;
        Ok((id, true))
    }

    async fn find_agent_creator(&self) -> Result<Option<ConversationHeader>, String> {
        let headers = self.store.list().await.map_err(|e| e.to_string())?;
        Ok(pick_oldest(headers.iter().filter(|h| h.agent_creator)).cloned())
    }

    async fn find_agent_session(
        &self,
        profile: &str,
        title: &str,
    ) -> Result<Option<ConversationHeader>, String> {
        let headers = self.store.list().await.map_err(|e| e.to_string())?;
        Ok(pick_oldest(
            headers
                .iter()
                .filter(|h| header_matches_agent(h, profile, title)),
        )
        .cloned())
    }

    async fn seed_opener_if_blank(&self, id: Ulid) -> Result<(), String> {
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
                    message: Message::assistant(AGENT_CREATOR_OPENER),
                    model: None,
                    reasoning: None,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Face-agent runtime: built-in `delegate` is never risk-gated by MCP name (it is core).
    /// Optional `"main-agent"` MCP grants are scoped + risk-gated separately so operators can
    /// thicken the surface without exposing the fleet by default.
    ///
    /// `turn_deferral` is the per-turn flag a `delegate` raises when its subagent deferred the
    /// action to the human out-of-band — read back by [`turn`](Self::turn) to drop the redundant
    /// reply (Gap 2). Privilege gate A: `create_agent` only when `profile` is an agent-creator
    /// and the weak self-handle is installed.
    pub(super) fn build_face_runtime(
        &self,
        user: &str,
        session: Ulid,
        capabilities: CapabilitySet,
        turn_deferral: Arc<AtomicBool>,
        profile: Option<&str>,
    ) -> Box<dyn ToolRuntime> {
        let extras = self.scoped_extras_runtime(user, session, capabilities);
        let agent_spawner = self.agent_spawner_for_profile(profile);
        Box::new(FaceRuntime::new(
            self.face_bridge.clone(),
            extras,
            Some(session.to_string()),
            turn_deferral,
            agent_spawner,
        ))
    }
}

#[async_trait]
impl AgentSpawner for ChatSessions {
    async fn spawn_agent(
        &self,
        profile: &str,
        title: Option<String>,
    ) -> Result<CreateAgentResult, String> {
        self.create_agent_chat(profile, title).await
    }
}

#[cfg(test)]
#[path = "agent_spawn_tests.rs"]
mod tests;
