//! Soft Chats | Agents shelf partition for the WebUI sidebar.
//!
//! Client-side only — see `docs/spec/architecture/chat-agent-surface-mode.md` §4 S2.

use chat_client_contract::{ConvHeader, ConversationSearchResult, SurfaceMode};

/// Which shelf the sidebar list is showing. Soft client-side taxonomy only —
/// see `docs/spec/architecture/chat-agent-surface-mode.md` §4 S2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Shelf {
    #[default]
    Chats,
    Agents,
}

impl Shelf {
    pub(super) fn surface_mode(self) -> SurfaceMode {
        match self {
            Shelf::Chats => SurfaceMode::Chat,
            Shelf::Agents => SurfaceMode::Agent,
        }
    }

    #[cfg(test)]
    pub(super) fn label(self) -> &'static str {
        match self {
            Shelf::Chats => "Chats",
            Shelf::Agents => "Agents",
        }
    }

    pub(super) fn empty_message(self) -> &'static str {
        match self {
            Shelf::Chats => "No chats yet.",
            Shelf::Agents => "No agents yet.",
        }
    }

    /// What the compact + control does. Chats starts a fresh chat; Agents opens
    /// the existing New Agent picker and does not bump the chat nonce.
    pub(super) fn create_action(self) -> ShelfCreate {
        match self {
            Shelf::Chats => ShelfCreate::FreshChat,
            Shelf::Agents => ShelfCreate::NewAgent,
        }
    }

    /// `title` / accessible name for the + control.
    pub(super) fn create_label(self) -> &'static str {
        match self {
            Shelf::Chats => "New chat",
            Shelf::Agents => "New agent",
        }
    }

    /// `title` / accessible name for the magnifying-glass control while search is closed.
    pub(super) fn search_button_label(self) -> &'static str {
        match self {
            Shelf::Chats => "Search chats",
            Shelf::Agents => "Search agents",
        }
    }

    /// Accessible name for the search field. The request is still
    /// `GET /api/conversations/search` (message grep); the shelf only decides
    /// which conversations those hits are allowed to show.
    pub(super) fn search_field_label(self) -> &'static str {
        match self {
            Shelf::Chats => "Search messages in chats",
            Shelf::Agents => "Search messages in agent sessions",
        }
    }

    pub(super) fn search_placeholder(self) -> &'static str {
        match self {
            Shelf::Chats => "Search chats...",
            Shelf::Agents => "Search agents...",
        }
    }
}

/// What the sidebar + control does for the active shelf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShelfCreate {
    /// Bump `new_chat_nonce` and clear the active conversation.
    /// Does not open the New Agent picker.
    FreshChat,
    /// Open the New Agent picker. Does not start a fresh Chats-shelf chat.
    NewAgent,
}

/// Search hits the list only while the field is open and the query has text.
/// Closing the field (or leaving it blank) shows the shelf again.
pub(super) fn search_is_active(open: bool, query: &str) -> bool {
    open && !query.trim().is_empty()
}

/// Partition the conversation list onto the active shelf. Soft filter only —
/// the full list stays in memory so switching shelves never drops selection.
pub(super) fn conversations_on_shelf<'a>(
    list: &'a [ConvHeader],
    shelf: Shelf,
) -> Vec<&'a ConvHeader> {
    let want = shelf.surface_mode();
    list.iter().filter(|c| c.surface_mode == want).collect()
}

/// Search hits have no `surface_mode` on the wire; scope them to the active
/// shelf by looking the id up in the already-fetched conversation headers.
/// Unknown ids (race / stale) stay visible so a hit is never silently dropped.
pub(super) fn search_results_on_shelf<'a>(
    results: &'a [ConversationSearchResult],
    conversations: &[ConvHeader],
    shelf: Shelf,
) -> Vec<&'a ConversationSearchResult> {
    let want = shelf.surface_mode();
    results
        .iter()
        .filter(
            |r| match conversations.iter().find(|c| c.id == r.conversation_id) {
                Some(c) => c.surface_mode == want,
                None => true,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(id: &str, mode: SurfaceMode) -> ConvHeader {
        ConvHeader {
            id: id.into(),
            title: Some(id.into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            surface_mode: mode,
            ..Default::default()
        }
    }

    /// The Chats shelf keeps only `SurfaceMode::Chat` rows; Agents keeps Agent.
    #[test]
    fn shelf_filter_partitions_by_surface_mode() {
        let list = vec![
            hdr("c1", SurfaceMode::Chat),
            hdr("a1", SurfaceMode::Agent),
            hdr("c2", SurfaceMode::Chat),
        ];
        let chats: Vec<_> = conversations_on_shelf(&list, Shelf::Chats)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();
        let agents: Vec<_> = conversations_on_shelf(&list, Shelf::Agents)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(chats, vec!["c1", "c2"]);
        assert_eq!(agents, vec!["a1"]);
    }

    /// An empty Agents shelf is a calm empty list, not an error — filter returns [].
    #[test]
    fn empty_agents_shelf_is_empty_vec() {
        let list = vec![hdr("c1", SurfaceMode::Chat)];
        assert!(conversations_on_shelf(&list, Shelf::Agents).is_empty());
    }

    /// Default shelf is Chats (product default).
    #[test]
    fn default_shelf_is_chats() {
        assert_eq!(Shelf::default(), Shelf::Chats);
        assert_eq!(Shelf::Chats.label(), "Chats");
        assert_eq!(Shelf::Agents.empty_message(), "No agents yet.");
    }

    /// + follows the shelf: Chats starts a fresh chat, Agents opens the picker.
    #[test]
    fn create_action_follows_the_shelf() {
        assert_eq!(Shelf::Chats.create_action(), ShelfCreate::FreshChat);
        assert_eq!(Shelf::Agents.create_action(), ShelfCreate::NewAgent);
        assert_eq!(Shelf::Chats.create_label(), "New chat");
        assert_eq!(Shelf::Agents.create_label(), "New agent");
    }

    /// Search copy names the shelf. The field is still the message-search API.
    #[test]
    fn search_copy_follows_the_shelf() {
        assert_eq!(Shelf::Chats.search_button_label(), "Search chats");
        assert_eq!(Shelf::Agents.search_button_label(), "Search agents");
        assert_eq!(Shelf::Chats.search_placeholder(), "Search chats...");
        assert_eq!(Shelf::Agents.search_placeholder(), "Search agents...");
        assert_eq!(
            Shelf::Chats.search_field_label(),
            "Search messages in chats"
        );
        assert_eq!(
            Shelf::Agents.search_field_label(),
            "Search messages in agent sessions"
        );
    }

    /// A closed field ignores leftover text; a blank open field is not a search.
    #[test]
    fn closed_or_blank_search_is_inactive() {
        assert!(!search_is_active(false, "hello"));
        assert!(!search_is_active(false, "  hello  "));
        assert!(!search_is_active(true, ""));
        assert!(!search_is_active(true, "   "));
        assert!(search_is_active(true, " hello "));
    }

    /// Search scoping uses the conversation list's surface_mode; unknown ids stay visible.
    #[test]
    fn search_scoped_to_shelf_keeps_unknown_ids() {
        let conversations = vec![hdr("c1", SurfaceMode::Chat), hdr("a1", SurfaceMode::Agent)];
        let results = vec![
            ConversationSearchResult {
                conversation_id: "c1".into(),
                title: None,
                created_at: "2026-01-01T00:00:00Z".into(),
                matches: vec![],
            },
            ConversationSearchResult {
                conversation_id: "a1".into(),
                title: None,
                created_at: "2026-01-01T00:00:00Z".into(),
                matches: vec![],
            },
            ConversationSearchResult {
                conversation_id: "unknown".into(),
                title: None,
                created_at: "2026-01-01T00:00:00Z".into(),
                matches: vec![],
            },
        ];
        let on_chats: Vec<_> = search_results_on_shelf(&results, &conversations, Shelf::Chats)
            .into_iter()
            .map(|r| r.conversation_id.as_str())
            .collect();
        assert_eq!(on_chats, vec!["c1", "unknown"]);
    }
}
