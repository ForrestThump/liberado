//! The store seam the session kernel talks through (session-focus S5′).
//!
//! [`GoalSessionHub`](crate::GoalSessionHub) used to hold a concrete [`GoalSessionStore`]. It now
//! holds an `Arc<dyn SessionRecordStore>`, which is what lets a *different* storage engine back the
//! same kernel — specifically the converged `Session` store (D7), where a chat conversation and a
//! goal session are one thing on disk.
//!
//! # Why a trait rather than just swapping the type
//!
//! The converged store must hold provider [`Message`]s (a chat turn has to round-trip back to the
//! model) *and* [`SessionEvent`]s (what the pack did). But this crate is `kernel` and
//! `liberado-provider` sits below it — the kernel deliberately does not know what a `Message` is,
//! and the layer-rules gate enforces that. So the converged store cannot live here; it lives in the
//! `store` tier and reaches *down* to this trait.
//!
//! That gives one storage engine with two typed views: this trait (records + events, kernel types
//! only) and `ConversationStore` (nodes + messages). One log, one id space, two lenses — the split
//! is in the *types*, not in the storage, which is exactly the duplication S5′ set out to kill.
//!
//! [`Message`]: https://docs.rs/liberado-provider
//! [`GoalSessionStore`]: crate::GoalSessionStore

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::event::SessionEvent;
use crate::goal::{GoalResult, GoalSessionRecord, SessionStatus};

/// Everything the session kernel needs from a store. Deliberately narrow: the hub orchestrates
/// packs, it does not care how a transcript is laid out on disk.
#[async_trait]
pub trait SessionRecordStore: Send + Sync {
    /// Record a new session (status `Pending`), and open its event bus.
    async fn insert(&self, record: GoalSessionRecord);

    async fn get(&self, id: &str) -> Option<GoalSessionRecord>;

    /// All sessions, newest first.
    async fn list(&self) -> Vec<GoalSessionRecord>;

    /// This session's events so far, or `None` if the session is unknown.
    async fn events(&self, id: &str) -> Option<Vec<SessionEvent>>;

    /// Subscribe to live events, plus a catch-up snapshot of everything already emitted. Returning
    /// both together is what closes the gap where an event lands between a read and a subscribe.
    async fn subscribe(
        &self,
        id: &str,
    ) -> Option<(Vec<SessionEvent>, broadcast::Receiver<SessionEvent>)>;

    /// Append one event to the transcript and fan it out to subscribers. Implementations also
    /// derive `awaiting_input` from the event stream here, so a listing can badge a session that
    /// needs a human without scanning its log.
    async fn push_event(&self, event: SessionEvent);

    async fn set_status(&self, id: &str, status: SessionStatus);

    /// Terminal transition: final status + result, recorded together.
    async fn finish(&self, id: &str, status: SessionStatus, result: GoalResult);
}
