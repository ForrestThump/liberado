//! Create-time `surface_mode` stamp + conversation create (Reading B / Slice 1).
//!
//! Kept off `sessions.rs` so that file stays at its cyclomatic / ploc baseline.
//! Spec: `docs/spec/architecture/chat-agent-surface-mode.md`.

use liberado_conversation_store::{
    Author, NewConversation, NewNode, SurfaceMode, Ulid, is_agent_profile,
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
/// Slice 1 call sites pass `explicit: None`; the override arm is here so the
/// rule has one authority when the wire grows an explicit create field.
pub(super) fn stamp_surface_mode(
    grant: &SessionGrant,
    explicit: Option<SurfaceMode>,
) -> SurfaceMode {
    if let Some(mode) = explicit {
        return mode;
    }
    if grant.profile.as_deref().is_some_and(is_agent_profile) {
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
        let surface_mode = stamp_surface_mode(&grant, None);
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

    fn grant_with_profile(profile: Option<&str>) -> SessionGrant {
        SessionGrant {
            profile: profile.map(str::to_owned),
            ..SessionGrant::default()
        }
    }

    #[test]
    fn explicit_agent_wins_over_unprofiled_grant() {
        assert_eq!(
            stamp_surface_mode(&grant_with_profile(None), Some(SurfaceMode::Agent)),
            SurfaceMode::Agent
        );
    }

    #[test]
    fn agent_profile_stamps_agent_when_no_explicit() {
        assert_eq!(
            stamp_surface_mode(&grant_with_profile(Some("coding")), None),
            SurfaceMode::Agent
        );
    }

    #[test]
    fn unprofiled_defaults_to_chat() {
        assert_eq!(
            stamp_surface_mode(&grant_with_profile(None), None),
            SurfaceMode::Chat
        );
    }

    #[test]
    fn explicit_chat_wins_over_agent_profile() {
        assert_eq!(
            stamp_surface_mode(&grant_with_profile(Some("coding")), Some(SurfaceMode::Chat)),
            SurfaceMode::Chat
        );
    }
}
