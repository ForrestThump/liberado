//! Split from `tests.rs` for module-health boundaries.

use chrono::Utc;
use liberado_conversation_store::{Author, ConversationStore, MessageNode, NewNode, StoreError};
use liberado_provider::Message;
use liberado_session::SessionStatus;
use ulid::Ulid;

use crate::{NewSession, Record, SessionHeader, SessionStore, Visibility};

fn user_node(parent: Option<Ulid>, text: &str) -> NewNode {
    NewNode {
        parent_id: parent,
        author: Author::User,
        message: Message::user(text),
        model: None,
    }
}

fn write_session_log(dir: &std::path::Path, header: &SessionHeader, nodes: &[MessageNode]) {
    let path = dir.join(format!("{}.jsonl", header.id));
    let mut lines = vec![serde_json::to_string(&Record::Header(Box::new(header.clone()))).unwrap()];
    for node in nodes {
        lines.push(serde_json::to_string(&Record::Node(node.clone())).unwrap());
    }
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn chat_header(id: Ulid) -> SessionHeader {
    SessionHeader {
        id,
        title: None,
        goal: None,
        parent_session: None,
        spawned_by: None,
        correlation_id: None,
        visibility: Visibility::default(),
        grant: Default::default(),
        status: SessionStatus::Pending,
        created_at: Utc::now(),
        finished_at: None,
        result: None,
        awaiting_input: false,
        ephemeral: false,
    }
}

fn user_message_node(id: Ulid, conversation: Ulid, parent: Option<Ulid>) -> MessageNode {
    MessageNode {
        id,
        parent_id: parent,
        conversation_id: conversation,
        author: Author::User,
        created_at: Utc::now(),
        message: Message::user("x"),
        model: None,
    }
}

#[tokio::test]
async fn leaf_path_names_a_missing_session() {
    let store = SessionStore::new();
    let ghost = Ulid::new();
    let err = ConversationStore::leaf_path(&store, ghost, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "missing session must be NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn leaf_path_names_a_missing_node() {
    let store = SessionStore::new();
    let session = store.create_session(NewSession::default()).await;
    ConversationStore::append(&store, session.id, user_node(None, "root"))
        .await
        .unwrap();
    let err = ConversationStore::leaf_path(&store, session.id, Some(Ulid::new()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "missing node must be NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn leaf_path_of_an_empty_session_is_empty() {
    let store = SessionStore::new();
    let session = store.create_session(NewSession::default()).await;
    let path = ConversationStore::leaf_path(&store, session.id, None)
        .await
        .unwrap();
    assert!(path.is_empty(), "no nodes means no path, got {path:?}");
}

#[tokio::test]
async fn leaf_path_reports_a_parent_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let conversation = Ulid::new();
    let a = Ulid::new();
    let b = Ulid::new();
    write_session_log(
        dir.path(),
        &chat_header(conversation),
        &[
            user_message_node(a, conversation, Some(b)),
            user_message_node(b, conversation, Some(a)),
        ],
    );
    let store = SessionStore::open(dir.path()).await;
    let err = ConversationStore::leaf_path(&store, conversation, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Corrupt(_)),
        "a cycle must be Corrupt, got {err:?}"
    );
    assert!(err.to_string().contains("parent cycle"), "{err}");
}

#[tokio::test]
async fn leaf_path_reports_a_missing_parent() {
    let dir = tempfile::tempdir().unwrap();
    let conversation = Ulid::new();
    let child = Ulid::new();
    let ghost = Ulid::new();
    write_session_log(
        dir.path(),
        &chat_header(conversation),
        &[user_message_node(child, conversation, Some(ghost))],
    );
    let store = SessionStore::open(dir.path()).await;
    let err = ConversationStore::leaf_path(&store, conversation, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Corrupt(_)),
        "a missing parent must be Corrupt, got {err:?}"
    );
    assert!(err.to_string().contains("missing parent"), "{err}");
}
