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
