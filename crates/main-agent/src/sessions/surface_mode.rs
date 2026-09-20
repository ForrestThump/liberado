//! Create-time `surface_mode` stamp + conversation create (Reading B / Slice 1).
//!
//! Kept off `sessions.rs` so that file stays at its cyclomatic / ploc baseline.
//! Spec: `docs/spec/architecture/chat-agent-surface-mode.md`.

use liberado_conversation_store::{
    AgentProfiles, Author, NewConversation, NewNode, SurfaceMode, Ulid,
};
use liberado_provider::Message;
use liberado_session::SessionGrant;

use super::{ChatSessions, SessionResult};

/// Stamp the chat-surface shelf at conversation create.
///
/// Decision order (locked in the plan):
/// 1. Explicit override wins when present (Slice 2+ create signal).
/// 2. Else `grant.profile ∈ agent_profiles` → `Agent`.
/// 3. Else `Chat`.
///
/// `profiles` is the deploy-tunable `AgentProfiles` set; passing
/// `&AgentProfiles::default()` is the right answer for tests and call sites
/// without a deployment context. Slice 1 call sites pass `explicit: None`;
/// the override arm is here so the rule has one authority when the wire
/// grows an explicit create field.
pub(super) fn stamp_surface_mode(
    grant: &SessionGrant,
    explicit: Option<SurfaceMode>,
    profiles: &AgentProfiles,
) -> SurfaceMode {
    if let Some(mode) = explicit {
        return mode;
    }
    if grant
        .profile
        .as_deref()
        .is_some_and(|name| profiles.is_agent(name))
    {
        return SurfaceMode::Agent;
    }
    SurfaceMode::Chat
}

impl ChatSessions {
    /// Shared create path: stamp `surface_mode`, persist header + system root.
    pub(super) async fn create_conversation(
        &self,
        title: Option<String>,
        ephemeral: bool,
        visibility: liberado_session::Visibility,
        grant: SessionGrant,
    ) -> SessionResult<Ulid> {
        // Reading B: stamp from profile class at create — not goal.is_some().
        // `agent_profiles` is the deploy-tunable set; tests stick to the
        // conservative default by constructing `ChatSessions` without
        // `with_agent_profiles`.
        let surface_mode = stamp_surface_mode(&grant, None, &self.agent_profiles);
        let header = self
            .store
            .create(NewConversation {
                title,
                parent_conversation: None,
                spawned_by: None,
                ephemeral,
                visibility,
                grant,
                surface_mode,
            })
            .await?;
        self.store
            .append(
                header.id,
                NewNode {
                    parent_id: None,
                    author: Author::System,
                    message: Message::system(&self.system_prompt),
                    model: None,
                },
            )
            .await?;
        Ok(header.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conservative default set. Tests run against the same definition
    /// production uses when no `[chat]` section is present — pinning the
    /// default here catches accidental drift.
    fn default_profiles() -> AgentProfiles {
        AgentProfiles::default()
    }

    fn grant_with_profile(profile: Option<&str>) -> SessionGrant {
        SessionGrant {
            profile: profile.map(str::to_owned),
            ..SessionGrant::default()
        }
    }

    #[test]
    fn explicit_agent_wins_over_unprofiled_grant() {
        assert_eq!(
            stamp_surface_mode(
                &grant_with_profile(None),
                Some(SurfaceMode::Agent),
                &default_profiles(),
            ),
            SurfaceMode::Agent
        );
    }

    #[test]
    fn agent_profile_stamps_agent_when_no_explicit() {
        assert_eq!(
            stamp_surface_mode(
                &grant_with_profile(Some("coding")),
                None,
                &default_profiles(),
            ),
            SurfaceMode::Agent
        );
    }

    #[test]
    fn unprofiled_defaults_to_chat() {
        assert_eq!(
            stamp_surface_mode(&grant_with_profile(None), None, &default_profiles(),),
            SurfaceMode::Chat
        );
    }

    #[test]
    fn explicit_chat_wins_over_agent_profile() {
        assert_eq!(
            stamp_surface_mode(
                &grant_with_profile(Some("coding")),
                Some(SurfaceMode::Chat),
                &default_profiles(),
            ),
            SurfaceMode::Chat
        );
    }

    /// A deployment that lists no agent profiles turns every chat to `Chat`,
    /// even the previously-agent hats. That's the knob's "empty list" edge,
    /// and it has to look the same as it did when the list was implicit-empty
    /// before this knob existed (which is to say: it doesn't exist, because
    /// this knob didn't exist — the smallest behavioural contract is "removing
    /// `coding` from the list demotes `coding` chats back to `Chat`").
    #[test]
    fn empty_agent_profiles_demotes_every_profile_to_chat() {
        let empty = AgentProfiles::new(std::iter::empty::<String>());
        assert_eq!(
            stamp_surface_mode(&grant_with_profile(Some("coding")), None, &empty),
            SurfaceMode::Chat
        );
    }

    /// A deployment that adds a custom specialist (`designer`) gets
    /// `Agent` from that profile alone; the conservative default is
    /// unaffected for the rows it covers.
    #[test]
    fn custom_agent_profile_is_honoured() {
        let custom = AgentProfiles::new(["coding", "designer"]);
        assert_eq!(
            stamp_surface_mode(&grant_with_profile(Some("designer")), None, &custom),
            SurfaceMode::Agent
        );
        assert_eq!(
            stamp_surface_mode(&grant_with_profile(Some("life")), None, &custom),
            SurfaceMode::Chat,
            "removing `life` from the list demotes `life` chats back to chat"
        );
    }
}
