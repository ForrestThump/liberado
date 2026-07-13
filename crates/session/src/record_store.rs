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
//! model) *and* [`SessionEvent`]s (what the pack did). This crate holds neither: it is deliberately
//! **provider-agnostic** — it does not know what a `Message` is. So the converged store cannot live
//! here; it lives in the `store` tier and reaches *down* to this trait.
//!
//! To be precise about *why*, because an earlier version of this comment overstated it: the
//! layer-rules gate does **not** forbid this. Kernel crates may depend on `liberado-provider`, and
//! `orchestrator`, `executor`, `dispatcher` and `main-agent` all do. Keeping *this* crate free of it
//! is a choice, and it buys something real — a `DomainPackRunner` is not required to be
//! LLM-shaped. That is why a pack records a turn as [`TurnAuthor`](crate::TurnAuthor) + text
//! ([`SessionRecordStore::append_turn`]) rather than as a provider `Message`: the store is what
//! knows how to make one.
//!
//! That gives one storage engine with two typed views: this trait (records + events, kernel types
//! only) and `ConversationStore` (nodes + messages). One log, one id space, two lenses — the split
//! is in the *types*, not in the storage, which is exactly the duplication S5′ set out to kill.
//!
//! [`Message`]: https://docs.rs/liberado-provider
//! [`GoalSessionStore`]: crate::GoalSessionStore

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::event::SessionEvent;
use crate::goal::{GoalResult, GoalSessionRecord, SessionStatus};

/// Who produced a **turn** — a conversational message in a session's transcript.
///
/// Deliberately not `liberado_provider::Role`, and deliberately not
/// `liberado_conversation_store::Author`: this crate knows about neither, and a pack should not have
/// to be LLM-shaped to have a conversation. The store maps this onto whatever a message actually is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnAuthor {
    System,
    User,
    Assistant,
    Tool,
    /// A named participant — a subagent, a debate role, a specific pack phase.
    Named(String),
}

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

    /// Append one **turn** — a conversational message — to this session's transcript, parented onto
    /// whatever the session's newest node is.
    ///
    /// This is the difference between an *observation* and a *conversation*. A pack's tool call is
    /// an [`event`](SessionEvent): something that happened, worth watching, not something anyone
    /// said. Its clarifying question, and your answer to it, are **turns**: they are the dialogue,
    /// and they belong in the message DAG.
    ///
    /// Packs recorded only events until 2026-07-13, and it cost two things that both looked like
    /// separate features until you notice they are the same gap:
    ///
    /// * a coding session's intake Q&A was **not searchable** — `chat-search` matches message nodes,
    ///   and there were none;
    /// * a goal session **could not be forked** — forking copies a node prefix, and a flat event log
    ///   has no `parent_id` to branch from. Forking a *coding* session at its freeze point (contract
    ///   A vs contract B) is the valuable version of forking, and this is what unblocks it.
    ///
    /// The store parents the node itself, so a pack does not have to track its own leaf; a pack's
    /// transcript is linear.
    async fn append_turn(&self, session_id: &str, author: TurnAuthor, content: String);

    async fn set_status(&self, id: &str, status: SessionStatus);

    /// Terminal transition: final status + result, recorded together.
    async fn finish(&self, id: &str, status: SessionStatus, result: GoalResult);
}
