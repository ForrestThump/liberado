//! Profile chip and `/profile` are chat-surface controls.
//!
//! An Agent conversation keeps the profile it was created with. The header chip hides, and
//! `/profile` does not open the picker. Spec: `docs/spec/architecture/chat-agent-surface-mode.md` §6e.

use chat_client_contract::SurfaceMode;
use dioxus::prelude::*;

use super::chat::ChatMsg;
use super::slash_commands::handle_slash_command;

/// Note shown when `/profile` is typed on an Agent conversation.
pub(crate) const AGENT_PROFILE_LOCKED: &str =
    "This agent keeps its profile for the session. Change profiles from a chat.";

/// The chip is a Chats control. Hidden only on an Agent surface. Empty chat (`None`) stays visible.
pub(crate) fn profile_chip_visible(surface: Option<SurfaceMode>) -> bool {
    !matches!(surface, Some(SurfaceMode::Agent))
}

/// The picker renders only when the chip would, and only when something asked it to open.
pub(crate) fn show_profile_browser(open: bool, surface: Option<SurfaceMode>) -> bool {
    open && profile_chip_visible(surface)
}

/// `/profile` on an Agent surface is a no-op open. Chat and empty chat still open the picker.
pub(crate) fn maybe_open_profile_browser(surface: Option<SurfaceMode>, mut open: Signal<bool>) {
    if profile_chip_visible(surface) {
        open.set(true);
    }
}

pub(crate) fn set_surface(mut slot: Signal<Option<SurfaceMode>>, mode: Option<SurfaceMode>) {
    slot.set(mode);
}

/// First token, so `/profile set coding` is the same command as `/profile`.
pub(crate) fn is_profile_command(text: &str) -> bool {
    text.trim().split_whitespace().next() == Some("/profile")
}

/// Slash dispatch that refuses `/profile` on an Agent surface before the command layer runs.
pub(crate) async fn handle_slash_for_surface(
    text: &str,
    api_base: &str,
    session_id: Option<String>,
    sending: bool,
    message_count: usize,
    current_theme: &str,
    surface: Option<SurfaceMode>,
) -> (
    Vec<ChatMsg>,
    Option<String>,
    Vec<liberado_commands::CommandResult>,
) {
    if !profile_chip_visible(surface) && is_profile_command(text) {
        let msg = ChatMsg {
            role: "system",
            content: AGENT_PROFILE_LOCKED.to_string(),
            thinking_steps: Vec::new(),
            reasoning: None,
        };
        return (vec![msg], None, Vec::new());
    }
    handle_slash_command(
        text,
        api_base,
        session_id,
        sending,
        message_count,
        current_theme,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_commands::CommandResult;

    #[test]
    fn chip_hides_only_on_an_agent_surface() {
        assert!(profile_chip_visible(None));
        assert!(profile_chip_visible(Some(SurfaceMode::Chat)));
        assert!(!profile_chip_visible(Some(SurfaceMode::Agent)));
        assert!(!show_profile_browser(true, Some(SurfaceMode::Agent)));
        assert!(show_profile_browser(true, Some(SurfaceMode::Chat)));
        assert!(show_profile_browser(true, None));
        assert!(!show_profile_browser(false, Some(SurfaceMode::Chat)));
    }

    #[test]
    fn profile_command_is_the_first_token() {
        assert!(is_profile_command("  /profile  "));
        assert!(is_profile_command("/profile set coding"));
        assert!(!is_profile_command("/profiler"));
        assert!(!is_profile_command("/help"));
    }

    #[tokio::test]
    async fn profile_on_an_agent_does_not_open_the_browser() {
        let (msgs, session, results) = handle_slash_for_surface(
            "/profile",
            "http://daemon.test",
            None,
            false,
            0,
            "dark",
            Some(SurfaceMode::Agent),
        )
        .await;
        assert!(results.is_empty());
        assert!(session.is_none());
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, AGENT_PROFILE_LOCKED);
    }

    #[tokio::test]
    async fn profile_on_a_chat_still_opens_the_browser() {
        for surface in [Some(SurfaceMode::Chat), None] {
            let (_msgs, _session, results) = handle_slash_for_surface(
                "/profile",
                "http://daemon.test",
                None,
                false,
                0,
                "dark",
                surface,
            )
            .await;
            assert!(
                results
                    .iter()
                    .any(|r| matches!(r, CommandResult::OpenProfileBrowser)),
                "{surface:?}"
            );
        }
    }
}
