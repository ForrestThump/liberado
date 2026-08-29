//! Default title seeding from the first user line.

use super::super::*;
use super::test_fixtures::*;

#[test]
fn default_title_uses_first_nonempty_line() {
    assert_eq!(
        default_conversation_title("  hello world  \nsecond line"),
        "hello world"
    );
    assert_eq!(default_conversation_title("\n\n  hi  "), "hi");
    assert_eq!(default_conversation_title("   \n  "), "");
}

#[test]
fn default_title_collapses_whitespace_and_truncates() {
    assert_eq!(
        default_conversation_title("too   many\t spaces"),
        "too many spaces"
    );
    let long = "x".repeat(100);
    let t = default_conversation_title(&long);
    assert_eq!(t.chars().count(), 72);
    assert!(t.ends_with('…'));
}

#[tokio::test]
async fn turn_seeds_title_from_first_user_line() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("ok")]).await;
    let id = sessions.create(None).await.unwrap();
    sessions
        .turn(id, "Plan a trip to Lisbon\nwith details")
        .await
        .unwrap();
    let headers = sessions.list().await.unwrap();
    let h = headers.iter().find(|h| h.id == id).unwrap();
    assert_eq!(h.title.as_deref(), Some("Plan a trip to Lisbon"));
}

#[tokio::test]
async fn seed_does_not_overwrite_explicit_title() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("ok")]).await;
    let id = sessions.create(Some("Pinned name".into())).await.unwrap();
    sessions
        .turn(id, "this should not become the title")
        .await
        .unwrap();
    let header = sessions.list().await.unwrap();
    let h = header.iter().find(|h| h.id == id).unwrap();
    assert_eq!(h.title.as_deref(), Some("Pinned name"));
}

#[tokio::test]
async fn list_backfills_title_from_existing_user_message() {
    use liberado_conversation_store::{Author, ConversationStore, NewConversation, NewNode};
    use liberado_provider::Message;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    // Pre-seed era: header with no title + a user message already on disk.
    let header = store
        .create(NewConversation {
            title: None,
            parent_conversation: None,
            spawned_by: None,
            ephemeral: false,
            visibility: Default::default(),
            grant: Default::default(),
        })
        .await
        .unwrap();
    store
        .append(
            header.id,
            NewNode {
                parent_id: None,
                author: Author::User,
                message: Message::user("Buy milk and eggs"),
                model: None,
            },
        )
        .await
        .unwrap();

    let sessions = sessions_at(dir.path(), Vec::new()).await;
    let headers = sessions.list().await.unwrap();
    let h = headers.iter().find(|h| h.id == header.id).unwrap();
    assert_eq!(h.title.as_deref(), Some("Buy milk and eggs"));

    // Second list is a no-op overwrite of the same default (title already Some).
    let headers2 = sessions.list().await.unwrap();
    assert_eq!(
        headers2
            .iter()
            .find(|h| h.id == header.id)
            .unwrap()
            .title
            .as_deref(),
        Some("Buy milk and eggs")
    );
}
