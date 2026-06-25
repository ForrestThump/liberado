//! Persistence orchestration for chat: turns a stateless HTTP/TUI front into durable, session-keyed
//! conversations backed by a [`ConversationStore`].
//!
//! This is the one code path every chat host shares (the web server today, a TUI-hosting daemon
//! later), which is why it lives here and not in any server. The host stays a thin adapter — it
//! resolves a session id and calls [`ChatSessions::turn`] / [`turn_stream`](ChatSessions::turn_stream).
//!
//! Two properties make this safe without an in-memory cache:
//!
//! * **The store is the source of truth.** Every turn rehydrates the conversation from the store
//!   (the server holds no conversation state), so any host instance over the same store sees the
//!   same history — horizontal scaling and process restarts are free.
//! * **A turn persists only on success.** New messages are written *after* the turn returns `Ok`, so
//!   a cancelled or errored turn writes nothing. The in-memory [`Conversation`] already rolls a
//!   dropped streaming turn back to a clean history; pairing that with append-on-success means the
//!   store can never hold a half-finished turn (e.g. an assistant `tool_calls` with no results).
//!
//! We depend on the [`ConversationStore`] *trait*, never a concrete store: the composition root
//! injects the engine (JSONL today, SQLite/Postgres later) so it stays swappable.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use liberado_conversation_store::{
    Author, ConversationHeader, ConversationStore, NewConversation, NewNode, StoreError, Ulid,
};
use liberado_executor::{AgentEvent, ExecError, Executor, ToolRuntime};
use liberado_provider::Message;
use thiserror::Error;
use tokio::sync::mpsc::Sender;

use crate::{Conversation, DEFAULT_SYSTEM_PROMPT};

/// What can go wrong running a persisted turn: the agent loop failed, or the store did. Both are
/// transparent — the caller sees the underlying cause, not a wrapper.
#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Exec(#[from] ExecError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The result type shared by every [`ChatSessions`] operation.
pub type SessionResult<T> = Result<T, SessionError>;

/// Durable, session-keyed chat over a [`ConversationStore`]. One per host; cheap to share behind an
/// `Arc`. Holds no conversation state — each turn rehydrates from the store and persists its tail on
/// success.
pub struct ChatSessions {
    store: Arc<dyn ConversationStore>,
    executor: Executor,
    runtime: Arc<dyn ToolRuntime>,
    system_prompt: String,
    /// Per-session turn serialization — one turn at a time per conversation.
    locks: Mutex<HashMap<Ulid, Arc<tokio::sync::Mutex<()>>>>,
}

impl ChatSessions {
    /// Build over an injected store, executor, and tool runtime, using [`DEFAULT_SYSTEM_PROMPT`].
    pub fn new(
        store: Arc<dyn ConversationStore>,
        executor: Executor,
        runtime: Arc<dyn ToolRuntime>,
    ) -> Self {
        Self {
            store,
            executor,
            runtime,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// Override the system prompt written as the root node of new conversations.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Create a new conversation, writing the system prompt as its root node, and return its id.
    /// Persisting the prompt as the root (rather than re-injecting it on load) keeps the store the
    /// single source of truth for the whole history, system prompt included.
    pub async fn create(&self, title: Option<String>) -> SessionResult<Ulid> {
        let header = self
            .store
            .create(NewConversation {
                title,
                parent_conversation: None,
                spawned_by: None,
            })
            .await?;
        self.store
            .append(
                header.id,
                NewNode {
                    parent_id: None,
                    author: Author::System,
                    message: Message::system(&self.system_prompt),
                },
            )
            .await?;
        Ok(header.id)
    }

    /// One non-streaming turn: rehydrate, run the agent over the full history, and — on success —
    /// persist the turn's new messages. A failed turn (the `?` short-circuit) persists nothing.
    pub async fn turn(&self, session: Ulid, user: &str) -> SessionResult<String> {
        let lock = self.session_lock(session);
        let _guard = lock.lock().await;
        let (mut convo, parent_leaf) = self.load(session).await?;
        let before = convo.len();
        let reply = convo
            .turn(&self.executor, self.runtime.as_ref(), user)
            .await?;
        self.persist_tail(session, &convo.history()[before..], parent_leaf)
            .await?;
        Ok(reply)
    }

    /// Streaming variant of [`turn`](Self::turn): same rehydrate → run → persist-on-success path,
    /// but emits [`AgentEvent`]s over `events` as the turn runs.
    ///
    /// On cancellation the caller drops this whole future before `persist_tail` runs, so — exactly
    /// as in the non-streaming path — nothing is written. The in-memory rollback in
    /// [`Conversation::turn_stream`] keeps the local history clean too; together they guarantee a
    /// stopped turn is a no-op against the store.
    pub async fn turn_stream(
        &self,
        session: Ulid,
        user: &str,
        events: &Sender<AgentEvent>,
    ) -> SessionResult<()> {
        let lock = self.session_lock(session);
        let _guard = lock.lock().await;
        let (mut convo, parent_leaf) = self.load(session).await?;
        let before = convo.len();
        convo
            .turn_stream(&self.executor, self.runtime.as_ref(), user, events)
            .await?;
        self.persist_tail(session, &convo.history()[before..], parent_leaf)
            .await?;
        Ok(())
    }

    /// Every conversation header, newest first — the sidebar listing.
    pub async fn list(&self) -> SessionResult<Vec<ConversationHeader>> {
        Ok(self.store.list().await?)
    }

    /// The ordered message history of a session (system prompt first), for rendering a reopened
    /// conversation.
    pub async fn history(&self, session: Ulid) -> SessionResult<Vec<Message>> {
        let nodes = self.store.leaf_path(session, None).await?;
        Ok(nodes.into_iter().map(|n| n.message).collect())
    }

    /// Get-or-insert the per-session turn lock, so two turns on the same conversation serialize
    /// (and never interleave their appends) while different conversations run concurrently.
    fn session_lock(&self, session: Ulid) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap();
        locks
            .entry(session)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Rehydrate a [`Conversation`] from the store's current leaf path, returning it alongside the
    /// id of the leaf node — the parent the turn's first new message will hang off of.
    async fn load(&self, session: Ulid) -> SessionResult<(Conversation, Option<Ulid>)> {
        let nodes = self.store.leaf_path(session, None).await?;
        let parent_leaf = nodes.last().map(|n| n.id);
        let messages = nodes.into_iter().map(|n| n.message).collect();
        Ok((Conversation::from_history(messages), parent_leaf))
    }

    /// Append a turn's new messages as a linear chain off `parent`, threading each appended node's
    /// id as the next one's parent so the on-disk DAG stays a straight line for a linear chat.
    async fn persist_tail(
        &self,
        session: Ulid,
        new: &[Message],
        mut parent: Option<Ulid>,
    ) -> SessionResult<()> {
        for msg in new {
            let node = self
                .store
                .append(
                    session,
                    NewNode {
                        parent_id: parent,
                        author: Author::from_role(msg.role),
                        message: msg.clone(),
                    },
                )
                .await?;
            parent = Some(node.id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use liberado_conversation_store::JsonlStore;
    use liberado_executor::Budget;
    use liberado_provider::{
        CompletionRequest, CompletionResponse, MockProvider, Provider, ProviderResult, Role,
        ToolDef, ToolInvocation,
    };

    struct NoTools;
    #[async_trait]
    impl ToolRuntime for NoTools {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Err("no tools".into())
        }
    }

    /// A provider whose completion never resolves — lets a turn get started then hang, so a test can
    /// cancel it mid-flight by dropping the turn future.
    struct PendingProvider;
    #[async_trait]
    impl Provider for PendingProvider {
        fn model(&self) -> &str {
            "pending"
        }
        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> ProviderResult<CompletionResponse> {
            std::future::pending().await
        }
    }

    /// A `ChatSessions` over a JSONL store at `root`, scripted with `replies` and no tools.
    fn sessions_at(root: &std::path::Path, replies: Vec<CompletionResponse>) -> ChatSessions {
        let store = Arc::new(JsonlStore::new(root));
        let provider = Arc::new(MockProvider::with_script("mock", replies));
        let executor = Executor::new(provider, Budget::default());
        ChatSessions::new(store, executor, Arc::new(NoTools))
    }

    #[tokio::test]
    async fn persisted_turn_round_trips_to_disk() {
        let dir = tempfile::tempdir().unwrap();

        let id = {
            let sessions = sessions_at(
                dir.path(),
                vec![CompletionResponse::text("Hi! How can I help?")],
            );
            let id = sessions.create(None).await.unwrap();
            let reply = sessions.turn(id, "hello").await.unwrap();
            assert_eq!(reply, "Hi! How can I help?");
            id
        };

        // A SECOND ChatSessions over the SAME store root must see the durable history: it round-trips
        // through disk, not an in-process cache.
        let reopened = sessions_at(dir.path(), Vec::new());
        let history = reopened.history(id).await.unwrap();
        assert_eq!(history[0].role, Role::System);
        assert!(
            history.iter().any(|m| m.content == "hello"),
            "user message did not persist"
        );
        assert!(
            history.iter().any(|m| m.content == "Hi! How can I help?"),
            "assistant reply did not persist"
        );
    }

    #[tokio::test]
    async fn context_carries_across_turns_via_rehydration() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JsonlStore::new(dir.path()));
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::text("Hi! How can I help?"),
                CompletionResponse::text("You said hello a moment ago."),
            ],
        ));
        let executor = Executor::new(provider.clone(), Budget::default());
        let sessions = ChatSessions::new(store, executor, Arc::new(NoTools));

        let id = sessions.create(None).await.unwrap();
        sessions.turn(id, "hello").await.unwrap();
        sessions.turn(id, "what did I just say?").await.unwrap();

        // The second turn rehydrated from the store, so its provider request carried the first user
        // message — context survived even though nothing was held in memory between turns.
        let second_request = &provider.received_requests()[1];
        assert!(
            second_request.messages.iter().any(|m| m.content == "hello"),
            "rehydration lost the first user message"
        );
    }

    #[tokio::test]
    async fn cancelled_stream_persists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JsonlStore::new(dir.path()));
        let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
        let sessions = ChatSessions::new(store, executor, Arc::new(NoTools));

        let id = sessions.create(None).await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(8);

        // Start the streaming turn, poll once to push the user message and issue the (hanging)
        // request, then drop the future to simulate the client stopping mid-turn.
        {
            let fut = sessions.turn_stream(id, "hi", &tx);
            tokio::pin!(fut);
            assert!(
                futures::poll!(fut.as_mut()).is_pending(),
                "the pending provider should leave the turn in flight"
            );
        } // fut dropped here

        // The store holds only the system prompt — the cancelled turn wrote nothing.
        let history = sessions.history(id).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, Role::System);
    }

    #[tokio::test]
    async fn list_returns_created_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = sessions_at(dir.path(), Vec::new());

        sessions.create(Some("My chat".into())).await.unwrap();
        let headers = sessions.list().await.unwrap();
        assert!(
            headers
                .iter()
                .any(|h| h.title.as_deref() == Some("My chat")),
            "list did not return the created conversation"
        );
    }
}
