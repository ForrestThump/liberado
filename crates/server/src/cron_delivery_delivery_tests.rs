//! Split from `cron_delivery.rs` for module-health boundaries.

use super::*;
use liberado_executor::Budget;
use liberado_session_store::SessionStore;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingNotifier(AtomicUsize);
#[async_trait]
impl Notifier for CountingNotifier {
    async fn notify(&self, _message: &str) -> Result<(), NotifyError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The brief must be folded into the sticky conversation *and* pushed: a notifier that
/// only sends loses the thread context; one that only appends never delivers.
#[tokio::test]
async fn deliver_cron_appends_to_the_sticky_session_and_sends() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = liberado_executor::Executor::new(
        Arc::new(liberado_provider::MockProvider::new("m")),
        Budget::default(),
    );
    let chat = Arc::new(ChatSessions::new(
        store.clone(),
        executor,
        Arc::new(NoTools),
    ));

    let notifier = ChatDeliveringNotifier::new(
        Arc::new(CountingNotifier(AtomicUsize::new(0))),
        chat,
        StickySession::ephemeral(),
        Arc::new(Mutex::new(None)), // never active → no quiet wait
        Duration::from_secs(300),
        Duration::from_secs(2700),
    );

    Notifier::deliver_cron(&notifier, "CRON-BRIEF-MARKER")
        .await
        .expect("delivery succeeds");

    use liberado_conversation_store::ConversationStore;
    let sticky_id = notifier.sticky.get().await.expect("sticky session created");
    let leaf = store.leaf_path(sticky_id, None).await.expect("readable");
    let last = leaf.last().expect("at least one node");
    assert!(
        last.message.content.contains("CRON-BRIEF-MARKER"),
        "the brief must be appended into the sticky conversation, got: {}",
        last.message.content
    );
    drop(notifier);
    let _ = NoTools;
}

struct NoTools;

#[async_trait]
impl liberado_executor::ToolRuntime for NoTools {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _: &liberado_provider::ToolInvocation) -> Result<String, String> {
        Err("no tools".into())
    }
}
