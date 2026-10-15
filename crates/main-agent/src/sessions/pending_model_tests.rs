//! The header `pending_model` is the durable form of a per-chat model pick.

use liberado_conversation_store::{Author, ChatMetaUpdate, ConversationStore};
use liberado_executor::{Budget, Executor};
use liberado_provider::CompletionResponse;

use super::super::*;
use super::test_fixtures::*;

fn sessions_with(
    store: Arc<SessionStore>,
    replies: Vec<CompletionResponse>,
) -> (ChatSessions, Arc<MockProvider>) {
    let provider = Arc::new(MockProvider::with_script("mock", replies));
    let sessions = ChatSessions::new(
        store,
        Executor::new(provider.clone(), Budget::default()),
        Arc::new(no_tools_runtime()),
    );
    (sessions, provider)
}

async fn user_models(sessions: &ChatSessions, id: Ulid) -> Vec<String> {
    sessions
        .history_nodes(id)
        .await
        .unwrap()
        .into_iter()
        .filter(|node| matches!(node.author, Author::User))
        .filter_map(|node| node.model)
        .collect()
}

#[tokio::test]
async fn a_stored_pending_model_is_stamped_exactly_and_then_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let (sessions, _) = sessions_with(store.clone(), vec![CompletionResponse::text("ok")]);
    let id = sessions.create(None).await.unwrap();
    store
        .set_chat_meta(
            id,
            ChatMetaUpdate {
                pending_model: Some(Some("vendor/exact-model".into())),
                ..ChatMetaUpdate::default()
            },
        )
        .await
        .unwrap();

    sessions.turn(id, "hello").await.unwrap();

    assert_eq!(
        user_models(&sessions, id).await,
        vec!["vendor/exact-model".to_string()]
    );
    assert!(store.header(id).await.unwrap().pending_model.is_none());
}

#[tokio::test]
async fn an_in_memory_pick_wins_and_the_stored_pick_does_not_apply_later() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let (sessions, _) = sessions_with(
        store.clone(),
        vec![
            CompletionResponse::text("one"),
            CompletionResponse::text("two"),
        ],
    );
    let id = sessions.create(None).await.unwrap();
    store
        .set_chat_meta(
            id,
            ChatMetaUpdate {
                pending_model: Some(Some("stored-model".into())),
                ..ChatMetaUpdate::default()
            },
        )
        .await
        .unwrap();
    sessions.select_model(id, "request-model".into());

    sessions.turn(id, "first").await.unwrap();
    assert_eq!(
        user_models(&sessions, id).await,
        vec!["request-model".to_string()]
    );
    assert!(store.header(id).await.unwrap().pending_model.is_none());

    sessions.turn(id, "second").await.unwrap();
    assert_eq!(
        user_models(&sessions, id).await,
        vec!["request-model".to_string(), "request-model".to_string()]
    );
}

#[tokio::test]
async fn asking_for_the_compaction_trigger_does_not_clear_a_stored_pick() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script("mock", []));
    let mut triggers = std::collections::HashMap::new();
    triggers.insert("picked-model".into(), 31_337u32);
    let sessions = ChatSessions::new(
        store.clone(),
        Executor::new(provider.clone(), Budget::default()),
        Arc::new(no_tools_runtime()),
    )
    .with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 11_111,
            model_trigger_tokens: triggers,
            unknown_model_trigger_tokens: 1_000_000,
            ..CompactionConfig::default()
        },
        provider,
    );
    let id = sessions.create(None).await.unwrap();
    store
        .set_chat_meta(
            id,
            ChatMetaUpdate {
                pending_model: Some(Some("picked-model".into())),
                ..ChatMetaUpdate::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        sessions.compaction_trigger_for_session(id).await,
        Some(31_337)
    );
    assert_eq!(
        sessions.compaction_trigger_for_session(id).await,
        Some(31_337)
    );
    assert_eq!(
        store.header(id).await.unwrap().pending_model.as_deref(),
        Some("picked-model")
    );
}
