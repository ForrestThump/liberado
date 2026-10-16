//! Sidebar rules for the Automation Editor singleton.
//!
//! The row is not a normal chat or agent. Search drops it too. If a row is
//! still drawn, Delete is blocked the same way as the Agent Creator.

use chat_client_contract::{ConvHeader, ConversationSearchResult, SurfaceMode};

/// Shown when Delete is disabled on this row. The server also returns 409.
pub(super) const EDITOR_DELETE_REASON: &str = "The Automation Editor cannot be deleted.";

pub(super) fn editor_delete_block(automation_editor: bool) -> Option<&'static str> {
    if automation_editor {
        Some(EDITOR_DELETE_REASON)
    } else {
        None
    }
}

/// Creator block wins when both flags are set. The editor block is the other arm.
pub(super) fn with_editor_block(
    creator_block: Option<&'static str>,
    id: &str,
    editor_ids: &[String],
) -> Option<&'static str> {
    creator_block.or_else(|| editor_delete_block(id_is_editor(id, editor_ids)))
}

pub(super) fn id_is_editor(id: &str, editor_ids: &[String]) -> bool {
    editor_ids.iter().any(|row| row == id)
}

/// False for the editor row, so it is left off the Chats and Agents shelves.
pub(super) fn listed_in_sidebar(id: &str, editor_ids: &[String]) -> bool {
    !id_is_editor(id, editor_ids)
}

pub(super) fn push_editor_id(item: &serde_json::Value, id: &str, editor_ids: &mut Vec<String>) {
    let flagged = item
        .get("automation_editor")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if flagged {
        editor_ids.push(id.to_string());
    }
}

pub(super) fn without_editor<'a>(
    rows: Vec<&'a ConvHeader>,
    editor_ids: &[String],
) -> Vec<&'a ConvHeader> {
    rows.into_iter()
        .filter(|row| listed_in_sidebar(&row.id, editor_ids))
        .collect()
}

pub(super) fn headers_without_editor(
    headers: &[ConvHeader],
    editor_ids: &[String],
) -> Vec<ConvHeader> {
    headers
        .iter()
        .filter(|row| listed_in_sidebar(&row.id, editor_ids))
        .cloned()
        .collect()
}

pub(super) fn search_hits(
    hits: Vec<&ConversationSearchResult>,
    editor_ids: &[String],
) -> Vec<ConversationSearchResult> {
    hits.into_iter()
        .filter(|hit| listed_in_sidebar(&hit.conversation_id, editor_ids))
        .cloned()
        .collect()
}

/// Shelf partition that also drops the editor, whatever its surface stamp is.
pub(super) fn rows_on_shelf<'a>(
    headers: &'a [ConvHeader],
    mode: SurfaceMode,
    editor_ids: &[String],
) -> Vec<&'a ConvHeader> {
    headers
        .iter()
        .filter(|row| row.surface_mode == mode && listed_in_sidebar(&row.id, editor_ids))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(id: &str, mode: SurfaceMode) -> ConvHeader {
        ConvHeader {
            id: id.into(),
            title: Some(id.into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            surface_mode: mode,
            ..Default::default()
        }
    }

    fn hit(id: &str) -> ConversationSearchResult {
        ConversationSearchResult {
            conversation_id: id.into(),
            title: Some(id.into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            matches: Vec::new(),
        }
    }

    #[test]
    fn the_editor_delete_control_is_blocked() {
        assert_eq!(editor_delete_block(false), None);
        assert_eq!(editor_delete_block(true), Some(EDITOR_DELETE_REASON));
        assert!(EDITOR_DELETE_REASON.contains("Automation Editor"));
        let editors = vec!["01EDITOR".to_string()];
        assert_eq!(
            with_editor_block(None, "01EDITOR", &editors),
            Some(EDITOR_DELETE_REASON)
        );
        assert_eq!(with_editor_block(None, "01CHAT", &editors), None);
        assert_eq!(
            with_editor_block(Some("creator"), "01EDITOR", &editors),
            Some("creator")
        );
        assert!(!listed_in_sidebar("01EDITOR", &editors));
        assert!(listed_in_sidebar("01CHAT", &editors));
    }

    #[test]
    fn the_editor_row_is_left_off_the_shelves_and_search() {
        let headers = vec![
            header("01EDITOR", SurfaceMode::Chat),
            header("01CHAT", SurfaceMode::Chat),
            header("01AGENT", SurfaceMode::Agent),
        ];
        let editors = vec!["01EDITOR".to_string()];
        let chats = rows_on_shelf(&headers, SurfaceMode::Chat, &editors);
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].id, "01CHAT");
        let agents = rows_on_shelf(&headers, SurfaceMode::Agent, &editors);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "01AGENT");
        let hits = search_hits(vec![&hit("01EDITOR"), &hit("01CHAT")], &editors);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_id, "01CHAT");
        assert_eq!(headers_without_editor(&headers, &editors).len(), 2);
    }

    #[test]
    fn the_list_payload_records_the_editor_flag() {
        let mut editors = Vec::new();
        let item = serde_json::json!({ "automation_editor": true });
        push_editor_id(&item, "01EDITOR", &mut editors);
        push_editor_id(&serde_json::json!({}), "01CHAT", &mut editors);
        push_editor_id(
            &serde_json::json!({ "automation_editor": false }),
            "01PLAIN",
            &mut editors,
        );
        assert_eq!(editors, vec!["01EDITOR".to_string()]);
    }
}
