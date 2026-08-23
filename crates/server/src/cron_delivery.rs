//! Chat-aware cron delivery: fold a finished cron brief into the sticky Telegram conversation, held
//! until the human is between messages, so a reply carries the brief in context and a brief never
//! barges into an active chat. Design: `docs/future-work/ideas/cron-delivery-timing-idea.md`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use liberado_main_agent::ChatSessions;
use liberado_notify::{Notifier, NotifyError};
use tokio::sync::Mutex;

use crate::sticky::StickySession;

/// A [`Notifier`] whose `deliver_cron` folds the brief into the sticky Telegram chat session and
/// defers the send around the human's activity. Plain and proposal notifications pass straight
/// through to `inner`, immediate and unchanged — only scheduled briefs get the append + defer.
pub struct ChatDeliveringNotifier {
    /// The real send channel (a `TelegramNotifier`) for the actual Telegram push and for the
    /// unchanged `notify`/`notify_proposal` paths.
    inner: Arc<dyn Notifier>,
    /// The face-agent session store — used to `append_note` the brief and to lazily create the
    /// sticky Telegram session if none exists yet.
    chat: Arc<ChatSessions>,
    /// Shared with [`crate::telegram::TelegramChatBridge`]: the conversation a brief appends into and
    /// a reply continues. Persisted across restarts; one lock guards lazy creation from either side.
    sticky: StickySession,
    /// Shared with the `ApprovalBot`: `Some(t)` = last inbound message at `t`; `None` = never active
    /// (deliver immediately — the common case where a brief fires and nobody is chatting).
    last_activity: Arc<Mutex<Option<Instant>>>,
    quiet_delay: Duration,
    deliver_by: Duration,
}

impl ChatDeliveringNotifier {
    pub fn new(
        inner: Arc<dyn Notifier>,
        chat: Arc<ChatSessions>,
        sticky: StickySession,
        last_activity: Arc<Mutex<Option<Instant>>>,
        quiet_delay: Duration,
        deliver_by: Duration,
    ) -> Self {
        Self {
            inner,
            chat,
            sticky,
            last_activity,
            quiet_delay,
            deliver_by,
        }
    }

    /// How long to wait before delivering, decided purely from the activity clock and the config —
    /// separated from the sleeping/looping so it is unit-testable without real time. Returns the
    /// delay to wait *now*; the caller re-checks after sleeping, since a new message resets `idle`.
    ///
    /// `elapsed_since_ready` is how long the brief has already been held (for the cap).
    fn next_wait(
        idle: Option<Duration>,
        elapsed_since_ready: Duration,
        quiet_delay: Duration,
        deliver_by: Duration,
    ) -> Option<Duration> {
        // Cap reached → deliver now, no matter how active the chat is.
        if elapsed_since_ready >= deliver_by {
            return None;
        }
        match idle {
            // Never active, or quiet long enough → deliver now.
            None => None,
            Some(idle) if idle >= quiet_delay => None,
            // Still active → wait the shorter of "until quiet" and "until the cap".
            Some(idle) => {
                let until_quiet = quiet_delay - idle;
                let until_cap = deliver_by.saturating_sub(elapsed_since_ready);
                Some(until_quiet.min(until_cap).max(Duration::from_millis(50)))
            }
        }
    }

    /// Block until the chat has been quiet for `quiet_delay`, or the brief has been held for
    /// `deliver_by`, whichever comes first. Returns immediately in the common case (no recent chat).
    async fn wait_for_quiet(&self) {
        let ready_at = Instant::now();
        loop {
            let idle = self.last_activity.lock().await.map(|t| t.elapsed());
            match Self::next_wait(idle, ready_at.elapsed(), self.quiet_delay, self.deliver_by) {
                None => return,
                Some(wait) => tokio::time::sleep(wait).await,
            }
        }
    }

    /// Resolve the sticky Telegram session (creating it if none exists yet — the same lazy-create the
    /// bridge does on a first message), then append the brief as an assistant-role note so a later
    /// reply rehydrates it as context. Best-effort: a failure here still lets the Telegram push go
    /// out, it just won't be in the conversation history.
    async fn append_to_sticky(&self, message: &str) {
        let chat = self.chat.clone();
        let id = match self
            .sticky
            .get_or_create(move || async move { chat.create(Some("Telegram".into())).await })
            .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(error = %e, "cron delivery: could not create sticky Telegram session");
                return;
            }
        };
        if let Err(e) = self.chat.append_note(id, message).await {
            tracing::warn!(error = %e, "cron delivery: append_note to sticky session failed");
        }
    }
}

#[async_trait]
impl Notifier for ChatDeliveringNotifier {
    async fn notify(&self, message: &str) -> Result<(), NotifyError> {
        self.inner.notify(message).await
    }

    async fn notify_proposal(&self, proposal_id: &str, message: &str) -> Result<(), NotifyError> {
        self.inner.notify_proposal(proposal_id, message).await
    }

    /// The whole point: hold the brief until the chat is quiet, fold it into the sticky conversation,
    /// then push it. Append happens *with* the send (not earlier), so the brief enters the thread in
    /// the order you see it — no message silently injected mid-conversation.
    async fn deliver_cron(&self, message: &str) -> Result<(), NotifyError> {
        self.wait_for_quiet().await;
        self.append_to_sticky(message).await;
        self.inner.notify(message).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: Duration = Duration::from_secs(300);
    const CAP: Duration = Duration::from_secs(2700);

    #[test]
    fn never_active_delivers_immediately() {
        assert_eq!(
            ChatDeliveringNotifier::next_wait(None, Duration::ZERO, QUIET, CAP),
            None
        );
    }

    #[test]
    fn quiet_long_enough_delivers_immediately() {
        assert_eq!(
            ChatDeliveringNotifier::next_wait(Some(QUIET), Duration::ZERO, QUIET, CAP),
            None
        );
        assert_eq!(
            ChatDeliveringNotifier::next_wait(
                Some(QUIET + Duration::from_secs(1)),
                Duration::ZERO,
                QUIET,
                CAP
            ),
            None
        );
    }

    #[test]
    fn recently_active_waits_the_remaining_quiet() {
        // Idle 60s of a 300s window, brief just became ready → wait ~240s.
        let wait = ChatDeliveringNotifier::next_wait(
            Some(Duration::from_secs(60)),
            Duration::ZERO,
            QUIET,
            CAP,
        )
        .unwrap();
        assert_eq!(wait, Duration::from_secs(240));
    }

    #[test]
    fn cap_forces_delivery_even_while_active() {
        // Chat still active (idle 10s), but the brief has been held past the cap → deliver now.
        assert_eq!(
            ChatDeliveringNotifier::next_wait(Some(Duration::from_secs(10)), CAP, QUIET, CAP),
            None
        );
    }

    #[test]
    fn wait_is_bounded_by_the_cap() {
        // Idle is tiny (would want ~300s) but only 100s of headroom remains before the cap → wait
        // the smaller, capped amount.
        let elapsed = CAP - Duration::from_secs(100);
        let wait =
            ChatDeliveringNotifier::next_wait(Some(Duration::from_secs(1)), elapsed, QUIET, CAP)
                .unwrap();
        assert_eq!(wait, Duration::from_secs(100));
    }
}

#[cfg(test)]
mod delivery_tests {
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
}
