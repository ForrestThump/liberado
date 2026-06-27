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
//!
//! # Slice 2 — runtime safety guards
//!
//! Each turn surfaces the full **capability-scoped** tool set: the runtime is wrapped in a
//! [`ScopedRuntime`] (limiting the model's visible tools to the granted MCPs) and a
//! [`RiskGatedToolRuntime`] (capability / consequence / magnitude checks). The model sees every
//! granted tool regardless of how the message is phrased — robust, with no missed requests. (An
//! earlier verb-keyword advisor was removed because it silently dropped legitimate requests phrased
//! without a listed verb, e.g. "what's on my calendar?".)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use liberado_common::{CapabilitySet, Consequence};
use liberado_conversation_store::{
    Author, ConversationHeader, ConversationStore, NewConversation, NewNode, StoreError, Ulid,
};
use liberado_executor::{AgentEvent, ExecError, Executor, ToolRuntime};
use liberado_mcp::{RiskGatedToolRuntime, ScopedRuntime};
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
///
/// When guard configuration is attached (via [`with_guards`](Self::with_guards)), each turn applies
/// the tool-advisor to select relevant MCPs and wraps the runtime in safety guards.
pub struct ChatSessions {
    store: Arc<dyn ConversationStore>,
    executor: Executor,
    runtime: Arc<dyn ToolRuntime>,
    system_prompt: String,
    /// Per-session turn serialization — one turn at a time per conversation.
    locks: Mutex<HashMap<Ulid, Arc<tokio::sync::Mutex<()>>>>,

    // ── Slice 2: runtime safety guards ──────────────────────────────────────
    /// `(mcp_name, consequence)` pairs for RiskGatedToolRuntime consequence gating.
    consequences: Vec<(String, Consequence)>,
    /// Capability grants for RiskGatedToolRuntime capability checking.
    capabilities: CapabilitySet,
    /// Directory under which `proposals/` subdirectory holds proposal files.
    proposals_dir: PathBuf,
}

impl ChatSessions {
    /// Build over an injected store, executor, and tool runtime, using [`DEFAULT_SYSTEM_PROMPT`].
    /// No safety guards are attached by default — call [`with_guards`](Self::with_guards) to enable
    /// the tool-advisor and RiskGatedToolRuntime for every turn.
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
            consequences: Vec::new(),
            capabilities: CapabilitySet::empty(),
            proposals_dir: PathBuf::new(),
        }
    }

    /// Override the system prompt written as the root node of new conversations.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Attach runtime safety guard configuration.
    ///
    /// When configured, each turn:
    /// 1. Scopes the runtime's visible tools to the granted MCPs (capability scoping).
    /// 2. Wraps in [`RiskGatedToolRuntime`] for capability / consequence / magnitude checks.
    ///
    /// # Arguments
    ///
    /// * `consequences` - `(mcp_name, consequence)` pairs for consequence gating.
    /// * `capabilities` - The base capability set for capability checks and tool scoping.
    /// * `proposals_dir` - Base directory for proposal files (`proposals/proposals/<id>.md`).
    pub fn with_guards(
        mut self,
        consequences: Vec<(String, Consequence)>,
        capabilities: CapabilitySet,
        proposals_dir: PathBuf,
    ) -> Self {
        self.consequences = consequences;
        self.capabilities = capabilities;
        self.proposals_dir = proposals_dir;
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
    ///
    /// When guard configuration is attached, the tool-advisor runs before the turn to select
    /// relevant MCPs, and the runtime is wrapped in [`RiskGatedToolRuntime`] for safety checks.
    pub async fn turn(&self, session: Ulid, user: &str) -> SessionResult<String> {
        let lock = self.session_lock(session);
        let _guard = lock.lock().await;
        let (mut convo, parent_leaf) = self.load(session).await?;
        let before = convo.len();

        let turn_runtime = self.build_turn_runtime(user, session);
        let reply = convo
            .turn(&self.executor, turn_runtime.as_ref(), user)
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
    ///
    /// When guard configuration is attached, the tool-advisor runs before the turn to select
    /// relevant MCPs, and the runtime is wrapped in [`RiskGatedToolRuntime`] for safety checks.
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

        let turn_runtime = self.build_turn_runtime(user, session);
        convo
            .turn_stream(&self.executor, turn_runtime.as_ref(), user, events)
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

    // ── private helpers ──────────────────────────────────────────────────────

    /// Build a per-turn [`ToolRuntime`] that scopes the visible tool surface to the granted
    /// capabilities and wraps the result in [`RiskGatedToolRuntime`] for capability / consequence /
    /// magnitude guards.
    ///
    /// When no guard configuration is attached, returns the raw `self.runtime` unchanged.
    fn build_turn_runtime(&self, user: &str, session: Ulid) -> Box<dyn ToolRuntime> {
        if self.capabilities.capabilities.is_empty() && self.consequences.is_empty() {
            // No guards configured — use the raw runtime directly.
            // We wrap in a pass-through box so the caller's interface stays uniform.
            return Box::new(PassThroughRuntime(self.runtime.clone()));
        }

        // TODO(tool-advisor): real on-demand surfacing — show a compact catalog (names+descriptions)
        // and lazy-load full tool schemas only when routed (the OpenClaw/Hermes lazy-load pattern) —
        // add this when the catalog is large enough that surfacing everything actually costs tokens.
        // The verb-list heuristic was removed because it silently dropped legitimate requests.

        // Capability scoping: surface only MCPs the chat agent is granted, every turn, regardless of
        // how the message is phrased. The model sees the full granted tool set (robust — no missed
        // requests). An empty grant set scopes to nothing (no tools visible).
        let granted_mcps: Vec<String> = self
            .capabilities
            .capabilities
            .iter()
            .filter_map(|c| match c {
                liberado_common::Capability::ExecuteMcp(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        // `ScopedRuntime` treats an empty allow-list as pass-through (its general-purpose default).
        // For capability scoping that's the wrong sense — no grants must mean no tools — so route the
        // empty case to a no-tools runtime instead of letting everything through.
        let inner: Arc<dyn ToolRuntime> = if granted_mcps.is_empty() {
            Arc::new(NoToolsRuntime)
        } else {
            Arc::new(ScopedRuntime::new(self.runtime.clone(), granted_mcps))
        };

        // Wrap in RiskGatedToolRuntime for safety guards (capability / consequence / magnitude).
        Box::new(RiskGatedToolRuntime::new(
            inner,
            self.capabilities.clone(),
            self.consequences.clone(),
            self.proposals_dir.clone(),
            user.to_string(),
            session.to_string(),
        ))
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

/// A thin pass-through wrapper that lets us return [`Arc<dyn ToolRuntime>`] as
/// [`Box<dyn ToolRuntime>`] when no guards are configured.
struct PassThroughRuntime(Arc<dyn ToolRuntime>);

#[async_trait::async_trait]
impl ToolRuntime for PassThroughRuntime {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        self.0.catalog()
    }

    async fn invoke(&self, call: &liberado_provider::ToolInvocation) -> Result<String, String> {
        self.0.invoke(call).await
    }
}

/// A runtime that exposes no tools — used when the chat agent holds no MCP grants, so the model is
/// shown an empty catalog (capability scoping, not `ScopedRuntime`'s empty-means-all default).
struct NoToolsRuntime;

#[async_trait::async_trait]
impl ToolRuntime for NoToolsRuntime {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        Vec::new()
    }

    async fn invoke(&self, _call: &liberado_provider::ToolInvocation) -> Result<String, String> {
        Err("no tools are granted to this chat agent".into())
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

    #[tokio::test]
    async fn guarded_turn_with_risk_gated_runtime_works() {
        // Verify that a ChatSessions with guards configured can still run a turn successfully.
        // The inner runtime has no tools, so the advisor should find nothing, and the turn
        // should complete as a pure conversation.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JsonlStore::new(dir.path()));
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text("Hello!")],
        ));
        let executor = Executor::new(provider, Budget::default());

        let sessions = ChatSessions::new(store, executor, Arc::new(NoTools))
            .with_guards(
                vec![("tasks-mcp".into(), Consequence::Reversible)],
                liberado_common::CapabilitySet::empty(),
                dir.path().join("proposals"),
            );

        let id = sessions.create(None).await.unwrap();
        let reply = sessions.turn(id, "hello").await.unwrap();
        assert_eq!(reply, "Hello!");
    }

    /// A runtime that always offers one tool, so we can assert what the model is shown.
    struct OneTool(&'static str);
    #[async_trait]
    impl ToolRuntime for OneTool {
        fn catalog(&self) -> Vec<ToolDef> {
            vec![ToolDef::new(
                self.0,
                "a tool",
                serde_json::json!({ "type": "object" }),
            )]
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }

    #[tokio::test]
    async fn granted_mcp_tools_surface_regardless_of_phrasing() {
        // A granted MCP's tools must be offered to the model even when the message is phrased
        // without an action verb (the case the removed verb-list advisor used to drop).
        use liberado_common::{Capability, CapabilitySet};

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JsonlStore::new(dir.path()));
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text("It's empty.")],
        ));
        let executor = Executor::new(provider.clone(), Budget::default());

        let sessions = ChatSessions::new(store, executor, Arc::new(OneTool("calendar-mcp:list")))
            .with_guards(
                vec![("calendar-mcp".into(), Consequence::Reversible)],
                CapabilitySet::from_iter([Capability::ExecuteMcp("calendar-mcp".into())]),
                dir.path().join("proposals"),
            );

        let id = sessions.create(None).await.unwrap();
        // No action verb — the old advisor would have surfaced zero tools here.
        sessions.turn(id, "what's on my calendar?").await.unwrap();

        let offered: Vec<String> = provider.received_requests()[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert!(
            offered.contains(&"calendar-mcp:list".to_string()),
            "granted MCP tool must be surfaced regardless of phrasing; got {offered:?}"
        );
    }

    #[tokio::test]
    async fn ungranted_mcp_tools_are_scoped_out() {
        // A configured-but-ungranted MCP must not be surfaced (capability scoping holds).
        use liberado_common::CapabilitySet;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JsonlStore::new(dir.path()));
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text("ok")],
        ));
        let executor = Executor::new(provider.clone(), Budget::default());

        let sessions = ChatSessions::new(store, executor, Arc::new(OneTool("email-mcp:send")))
            .with_guards(
                vec![("email-mcp".into(), Consequence::External)],
                CapabilitySet::empty(), // nothing granted
                dir.path().join("proposals"),
            );

        let id = sessions.create(None).await.unwrap();
        sessions.turn(id, "list my email").await.unwrap();

        let offered: Vec<String> = provider.received_requests()[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert!(
            offered.is_empty(),
            "ungranted MCP tools must be scoped out; got {offered:?}"
        );
    }
}
