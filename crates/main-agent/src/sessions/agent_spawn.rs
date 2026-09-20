//! Privileged Agents-shelf create path (face `create_agent` + shared resolver).
//!
//! Kept off `sessions.rs` for module-health. The face tool requests create; this
//! module resolves the named profile (fail closed) and calls `create_with_grant`
//! so Reading B stamps `Agent`. Not GoalSessionHub / `delegate`.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use liberado_common::CapabilitySet;
use liberado_conversation_store::{Ulid, is_agent_profile};
use liberado_executor::ToolRuntime;
use liberado_session::SessionGrant;

use super::ChatSessions;
use crate::face::{AgentSpawner, CreateAgentResult, FaceRuntime};

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

    /// Create a long-lived Agent-shelf chat under a named agent-eligible profile.
    ///
    /// Grant = only what the resolver returns for `profile`. Rejects non-agent
    /// names and missing resolver. Does not start a turn (opening messages are
    /// out of scope — return the id for the human/face to continue).
    pub async fn create_agent_chat(
        &self,
        profile: &str,
        title: Option<String>,
    ) -> Result<CreateAgentResult, String> {
        let profile = profile.trim();
        if profile.is_empty() {
            return Err("create_agent requires a non-empty profile".into());
        }
        if !is_agent_profile(profile) {
            return Err(format!(
                "profile `{profile}` is not agent-eligible (coding|life|researcher|operator)"
            ));
        }
        let resolver = self.profile_resolver.as_ref().ok_or_else(|| {
            "create_agent is not configured (no profile resolver wired)".to_string()
        })?;
        let grant = resolver(profile)?;
        let title_for_result = title.clone();
        let id = self
            .create_with_grant(title, grant)
            .await
            .map_err(|e| e.to_string())?;
        Ok(CreateAgentResult {
            conversation_id: id.to_string(),
            profile: profile.to_owned(),
            title: title_for_result,
        })
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
mod tests {
    use super::*;
    use liberado_conversation_store::ConversationStore;
    use liberado_executor::{Budget, Executor};
    use liberado_provider::MockProvider;
    use liberado_session_store::SessionStore;
    use liberado_test_support::NoopRuntime;

    #[tokio::test]
    async fn create_agent_chat_stamps_agent_via_grant() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path()).await);
        let sessions = Arc::new(
            ChatSessions::new(
                store.clone(),
                Executor::new(
                    Arc::new(MockProvider::with_script("m", vec![])),
                    Budget::default(),
                ),
                Arc::new(NoopRuntime),
            )
            .with_profile_resolver(Arc::new(|name: &str| {
                Ok(SessionGrant {
                    profile: Some(name.to_owned()),
                    ..SessionGrant::default()
                })
            })),
        );
        sessions.install_self_handle();
        let result = sessions
            .create_agent_chat("coding", Some("Coder".into()))
            .await
            .unwrap();
        assert_eq!(result.profile, "coding");
        let id: liberado_conversation_store::Ulid = result.conversation_id.parse().unwrap();
        let header = store.header(id).await.unwrap();
        assert_eq!(
            header.surface_mode,
            liberado_conversation_store::SurfaceMode::Agent
        );
        assert_eq!(header.grant.profile.as_deref(), Some("coding"));
    }

    #[tokio::test]
    async fn create_agent_chat_rejects_non_agent_profile() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path()).await);
        let sessions = ChatSessions::new(
            store,
            Executor::new(
                Arc::new(MockProvider::with_script("m", vec![])),
                Budget::default(),
            ),
            Arc::new(NoopRuntime),
        )
        .with_profile_resolver(Arc::new(|_| Ok(SessionGrant::default())));
        let err = sessions
            .create_agent_chat("chat-default", None)
            .await
            .unwrap_err();
        assert!(err.contains("not agent-eligible"), "{err}");
    }

    #[tokio::test]
    async fn non_creator_profile_gets_no_spawner_even_with_handle() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path()).await);
        let sessions = Arc::new(ChatSessions::new(
            store,
            Executor::new(
                Arc::new(MockProvider::with_script("m", vec![])),
                Budget::default(),
            ),
            Arc::new(NoopRuntime),
        ));
        sessions.install_self_handle();
        assert!(sessions.agent_spawner_for_profile(Some("coding")).is_none());
        assert!(
            sessions
                .agent_spawner_for_profile(Some("operator"))
                .is_some()
        );
        assert!(sessions.agent_spawner_for_profile(None).is_some());
    }
}
