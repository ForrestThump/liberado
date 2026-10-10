//! `wire_approvals` with and without chat, and with and without a notifier.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_conversation_store::{ConversationStore, Ulid};
use liberado_executor::Budget;
use liberado_main_agent::ChatSessions;
use liberado_notify::{Notifier, NotifyError};
use liberado_session_store::SessionStore;
use liberado_telegram_approvals::PermissionResolver;
use liberado_test_support::InvocationRecordingRuntime;
use liberado_vault::Vault;

use super::{ApprovalHub, wire_approvals};
use crate::sticky::StickySession;

struct Nop;

#[async_trait]
impl Notifier for Nop {
    async fn notify(&self, _message: &str) -> Result<(), NotifyError> {
        Ok(())
    }
}

struct World {
    _dir: tempfile::TempDir,
    store: Arc<SessionStore>,
    vault: Vault,
}

impl World {
    async fn open() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        let store = Arc::new(SessionStore::open(dir.path()).await);
        Self {
            _dir: dir,
            store,
            vault,
        }
    }

    fn chat(&self) -> Arc<ChatSessions> {
        let store: Arc<dyn ConversationStore> = self.store.clone();
        Arc::new(ChatSessions::new(
            store,
            liberado_executor::Executor::new(
                Arc::new(liberado_provider::MockProvider::new("m")),
                Budget::default(),
            ),
            Arc::new(InvocationRecordingRuntime::default()),
        ))
    }

    fn path(&self) -> &std::path::Path {
        self._dir.path()
    }
}

fn assert_wired(resolver: &Arc<PermissionResolver>, hub: &ApprovalHub) {
    assert!(Arc::ptr_eq(resolver, &hub.resolver));
    assert!(resolver.ledger().is_some());
}

async fn sticky_with_id() -> (StickySession, Ulid) {
    let sticky = StickySession::ephemeral();
    let id = Ulid::new();
    sticky.set(Some(id)).await;
    (sticky, id)
}

#[tokio::test]
async fn chat_with_a_notifier_gets_a_sink_and_the_sticky_id() {
    let world = World::open().await;
    let chat = world.chat();
    let (sticky, id) = sticky_with_id().await;
    let (resolver, hub) = wire_approvals(
        Some(&chat),
        &sticky,
        world.vault.clone(),
        world.path(),
        Some(Arc::new(Nop)),
    )
    .await;
    assert!(chat.has_permission_sink());
    assert_eq!(*chat.sticky_slot().lock().unwrap(), Some(id));
    assert_wired(&resolver, &hub);
}

#[tokio::test]
async fn chat_without_a_notifier_still_receives_the_sticky_id() {
    let world = World::open().await;
    let chat = world.chat();
    let (sticky, id) = sticky_with_id().await;
    let (resolver, hub) = wire_approvals(
        Some(&chat),
        &sticky,
        world.vault.clone(),
        world.path(),
        None,
    )
    .await;
    assert!(!chat.has_permission_sink());
    assert_eq!(*chat.sticky_slot().lock().unwrap(), Some(id));
    assert_wired(&resolver, &hub);
}

#[tokio::test]
async fn no_chat_does_not_publish_the_sticky_id() {
    let world = World::open().await;
    let unused = world.chat();
    let (sticky, _id) = sticky_with_id().await;
    let (resolver, hub) = wire_approvals(
        None,
        &sticky,
        world.vault.clone(),
        world.path(),
        Some(Arc::new(Nop)),
    )
    .await;
    assert!(!unused.has_permission_sink());
    assert_eq!(*unused.sticky_slot().lock().unwrap(), None);
    assert_wired(&resolver, &hub);
}

#[tokio::test]
async fn no_chat_and_no_notifier_still_builds_the_hub() {
    let world = World::open().await;
    let unused = world.chat();
    let (sticky, _id) = sticky_with_id().await;
    let (resolver, hub) =
        wire_approvals(None, &sticky, world.vault.clone(), world.path(), None).await;
    assert!(!unused.has_permission_sink());
    assert_eq!(*unused.sticky_slot().lock().unwrap(), None);
    assert_wired(&resolver, &hub);
}
