//! Project and pending-model fields on the session header.

use liberado_conversation_store::{ChatMetaUpdate, ConversationStore, NewConversation, Ulid};
use liberado_session_store::SessionStore;
use tempfile::tempdir;

fn chat() -> NewConversation {
    NewConversation {
        title: Some("a chat".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn project_and_pending_model_round_trip_and_a_fork_keeps_only_the_project() {
    let dir = tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await;
    let id = store.create(chat()).await.unwrap().id;

    store
        .set_chat_meta(
            id,
            ChatMetaUpdate {
                project: Some(Some("liberado".into())),
                pending_model: Some(Some("vendor/exact".into())),
            },
        )
        .await
        .unwrap();

    let header = store.header(id).await.unwrap();
    assert_eq!(header.project.as_deref(), Some("liberado"));
    assert_eq!(header.pending_model.as_deref(), Some("vendor/exact"));

    let reopened = SessionStore::open(dir.path()).await;
    let loaded = reopened.header(id).await.unwrap();
    assert_eq!(loaded.project.as_deref(), Some("liberado"));
    assert_eq!(loaded.pending_model.as_deref(), Some("vendor/exact"));

    store
        .append(
            id,
            liberado_conversation_store::NewNode {
                parent_id: None,
                author: liberado_conversation_store::Author::User,
                message: liberado_provider::Message::user("hello"),
                model: None,
                reasoning: None,
            },
        )
        .await
        .unwrap();
    let forked = store.fork_session(id, None, None).await.unwrap();
    assert_eq!(forked.project.as_deref(), Some("liberado"));
    assert!(forked.pending_model.is_none());

    store
        .set_chat_meta(
            id,
            ChatMetaUpdate {
                project: Some(None),
                pending_model: Some(None),
            },
        )
        .await
        .unwrap();
    let cleared = store.header(id).await.unwrap();
    assert!(cleared.project.is_none());
    assert!(cleared.pending_model.is_none());
}

#[tokio::test]
async fn chat_meta_on_a_missing_session_is_not_found() {
    let store = SessionStore::new();
    let err = store
        .set_chat_meta(Ulid::new(), ChatMetaUpdate::default())
        .await;
    // An empty update does not touch the store.
    assert!(err.is_ok());
    let err = store
        .set_chat_meta(
            Ulid::new(),
            ChatMetaUpdate {
                project: Some(Some("liberado".into())),
                pending_model: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        liberado_conversation_store::StoreError::NotFound(_)
    ));
}
