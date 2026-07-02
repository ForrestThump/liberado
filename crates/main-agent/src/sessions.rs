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
//!
//! # Dispatch routing
//!
//! When [`with_dispatch`](Self::with_dispatch) is attached, every turn is classified by a
//! [`Dispatcher`] *before* any execution happens — closing the gap where chat used to drive the
//! executor directly, bypassing the guard pipeline and sub-delegation entirely. The four
//! `DispatchAction` outcomes are handled asymmetrically, deliberately: `ExecuteDirect` (the common
//! case) falls straight through into the existing streaming `Conversation::turn`/`turn_stream`
//! path — zero change to today's token-by-token UX, now just gated on the dispatcher's approval.
//! `Clarify`, `Propose`, and `DispatchSubagent` all route through [`Orchestrator::run`], which has
//! no streaming variant (it calls the executor's report-mode `execute`, blocking until a full
//! `Report`) — for `DispatchSubagent` specifically this is an accepted, deliberate UX trade-off
//! (reserved for complex/open-ended goals, presumably rarer than direct execution) rather than an
//! oversight; a "working on it" status plus the final report stands in for live tokens on that one
//! path. `with_dispatch` requires an [`Orchestrator`] up front (not optional) because all three
//! non-`ExecuteDirect` outcomes need it to produce a `Disposition` — a chat host with no MCP
//! configured at all simply never calls `with_dispatch`, and turns run exactly as before.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use liberado_common::{CapabilityCatalog, CapabilitySet, Consequence, DispatchAction};
use liberado_conversation_store::{
    Author, ConversationHeader, ConversationStore, NewConversation, NewNode, StoreError, Ulid,
};
use liberado_dispatcher::{DispatchRequest, Dispatcher};
use liberado_executor::{AgentEvent, ExecError, Executor, RiskGatedToolRuntime, ToolRuntime};
use liberado_mcp::ScopedRuntime;
use liberado_orchestrator::{Disposition, Orchestrator};
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

    // ── Dispatch routing ─────────────────────────────────────────────────────
    /// When present, every turn is classified before execution. See the module docs.
    dispatcher: Option<Dispatcher>,
    /// The MCP catalog the dispatcher's classifier chooses from — the same shared, live catalog
    /// the daemon's reactive path and the server's API read, snapshotted fresh per dispatch call
    /// rather than frozen at construction.
    dispatch_catalog: Arc<CapabilityCatalog>,
    /// Executes non-`ExecuteDirect` decisions. Required alongside `dispatcher` — see
    /// [`with_dispatch`](Self::with_dispatch).
    orchestrator: Option<Orchestrator>,
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
            dispatcher: None,
            dispatch_catalog: Arc::new(CapabilityCatalog::new()),
            orchestrator: None,
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

    /// Attach dispatch routing (see the module docs). `catalog` is the shared, live MCP catalog
    /// the dispatcher's classifier chooses from — the same object the daemon's reactive path and
    /// the server's API read, snapshotted fresh per turn rather than frozen at construction.
    /// `orchestrator` executes the `Clarify`/`Propose`/`DispatchSubagent` outcomes (required —
    /// those three all need it to produce a `Disposition`; `ExecuteDirect` never touches it,
    /// staying on the streaming path).
    pub fn with_dispatch(
        mut self,
        dispatcher: Dispatcher,
        catalog: Arc<CapabilityCatalog>,
        orchestrator: Orchestrator,
    ) -> Self {
        self.dispatcher = Some(dispatcher);
        self.dispatch_catalog = catalog;
        self.orchestrator = Some(orchestrator);
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

        let reply = match self.dispatch_turn(user).await {
            DispatchOutcome::Answered(reply) => {
                convo.answer(user, &reply);
                reply
            }
            DispatchOutcome::Proceed(relevant_mcps) => {
                let turn_runtime = self.build_turn_runtime(user, session, &relevant_mcps);
                convo
                    .turn(&self.executor, turn_runtime.as_ref(), user)
                    .await?
            }
        };
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

        match self.dispatch_turn(user).await {
            DispatchOutcome::Answered(reply) => {
                convo.answer(user, &reply);
                // Deliver the already-resolved reply as a single token so it renders through the
                // existing SSE contract unchanged — no new event type needed.
                let _ = events.send(AgentEvent::Token(reply)).await;
            }
            DispatchOutcome::Proceed(relevant_mcps) => {
                let turn_runtime = self.build_turn_runtime(user, session, &relevant_mcps);
                convo
                    .turn_stream(&self.executor, turn_runtime.as_ref(), user, events)
                    .await?;
            }
        }
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

    /// Set the title of a conversation. Idempotent — subsequent calls overwrite the same field.
    pub async fn set_title(&self, session: Ulid, title: String) -> SessionResult<()> {
        Ok(self.store.set_title(session, title).await?)
    }

    // ── private helpers ──────────────────────────────────────────────────────

    /// Classify `user` via the dispatcher (when attached) and resolve everything except
    /// `ExecuteDirect` — which returns [`DispatchOutcome::Proceed`] (carrying the decision's
    /// `relevant_mcps`, if any) so the caller falls through to the normal streaming execution
    /// path, scoped by whatever narrowing the dispatcher found. See the module docs for why this
    /// split exists.
    async fn dispatch_turn(&self, user: &str) -> DispatchOutcome {
        let (Some(dispatcher), Some(orchestrator)) = (&self.dispatcher, &self.orchestrator) else {
            return DispatchOutcome::Proceed(Vec::new()); // no dispatcher — run exactly as before
        };

        let req = DispatchRequest {
            goal: user.to_string(),
            catalog: self.dispatch_catalog.descriptors(),
            capabilities: self.capabilities.clone(),
            reaction_depth: 0, // user-initiated, not a background reaction
        };
        let decision = match dispatcher.dispatch(&req).await {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(error = %e, "chat dispatch failed — proceeding without routing");
                return DispatchOutcome::Proceed(Vec::new());
            }
        };
        if let DispatchAction::ExecuteDirect { relevant_mcps, .. } = &decision.action {
            return DispatchOutcome::Proceed(relevant_mcps.clone());
        }

        let correlation_id = format!("chat-{}", Ulid::new());
        match orchestrator.run(decision, user, &correlation_id).await {
            Ok(Disposition::Clarify { questions, .. }) => {
                DispatchOutcome::Answered(format_questions(&questions))
            }
            Ok(Disposition::Reported(report)) => DispatchOutcome::Answered(report.summary),
            Ok(Disposition::Propose(proposal)) => match self.write_chat_proposal(&proposal).await
            {
                Ok(path) => DispatchOutcome::Answered(format!(
                    "I've drafted a proposal for you to review — it needs your approval before it \
                     runs: {}",
                    path.display()
                )),
                Err(e) => {
                    tracing::error!(error = %e, "failed to write chat proposal");
                    DispatchOutcome::Answered(
                        "I wanted to propose an action but couldn't save it — please try again."
                            .into(),
                    )
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "chat orchestration failed");
                DispatchOutcome::Answered(format!("I ran into a problem handling that: {e}"))
            }
        }
    }

    /// Write a dispatcher-originated proposal the same way [`RiskGatedToolRuntime`]'s runtime-level
    /// proposals are written: plain `tokio::fs::write` under `proposals_dir/proposals/`, not a
    /// vault write. Chat proposals live in the data dir (not the vault) so a vault watcher never
    /// reacts to them — a vault-resident proposal surface would need a provenance-tagged
    /// `Vault::write` (Decision 11), deferred same as the runtime-level ones.
    async fn write_chat_proposal(
        &self,
        proposal: &liberado_common::Proposal,
    ) -> std::io::Result<PathBuf> {
        let proposals_subdir = self.proposals_dir.join("proposals");
        let proposal_path = proposals_subdir.join(format!("{}.md", proposal.id));
        tokio::fs::create_dir_all(&proposals_subdir).await?;
        tokio::fs::write(&proposal_path, proposal.to_note()).await?;
        Ok(proposal_path)
    }

    /// Build a per-turn [`ToolRuntime`] that scopes the visible tool surface to the granted
    /// capabilities (further narrowed by `relevant_mcps` when the dispatcher supplied one — see
    /// [`dispatch_turn`](Self::dispatch_turn) and `DispatchTuning::narrow_direct_tools`) and wraps
    /// the result in [`RiskGatedToolRuntime`] for capability / consequence / magnitude guards.
    ///
    /// When no guard configuration is attached, returns the raw `self.runtime` unchanged.
    fn build_turn_runtime(
        &self,
        user: &str,
        session: Ulid,
        relevant_mcps: &[String],
    ) -> Box<dyn ToolRuntime> {
        if self.capabilities.capabilities.is_empty() && self.consequences.is_empty() {
            // No guards configured — use the raw runtime directly.
            // We wrap in a pass-through box so the caller's interface stays uniform.
            return Box::new(PassThroughRuntime(self.runtime.clone()));
        }

        // Capability scoping: surface only MCPs the chat agent is granted, every turn, regardless of
        // how the message is phrased. The model sees the full granted tool set (robust — no missed
        // requests). An empty grant set scopes to nothing (no tools visible).
        let granted_mcps: Vec<String> = self.capabilities.granted_mcps();
        // Dispatcher-narrowed tool surfacing (the token-efficiency piece — see module docs): when
        // the dispatch step named specific relevant MCPs for this goal, further narrow within the
        // granted ceiling instead of always surfacing every granted MCP's full tool schemas. Never
        // widens — only names already in `granted_mcps` survive the intersection.
        let scoped_mcps: Vec<String> = if relevant_mcps.is_empty() {
            granted_mcps
        } else {
            granted_mcps
                .into_iter()
                .filter(|name| relevant_mcps.contains(name))
                .collect()
        };
        // `ScopedRuntime` treats an empty allow-list as pass-through (its general-purpose default).
        // For capability scoping that's the wrong sense — no grants must mean no tools — so route the
        // empty case to a no-tools runtime instead of letting everything through.
        let inner: Arc<dyn ToolRuntime> = if scoped_mcps.is_empty() {
            Arc::new(NoToolsRuntime)
        } else {
            tracing::debug!(count = scoped_mcps.len(), mcps = ?scoped_mcps, "chat turn tool scope");
            Arc::new(ScopedRuntime::new(self.runtime.clone(), scoped_mcps))
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

/// What [`ChatSessions::dispatch_turn`] resolved to.
enum DispatchOutcome {
    /// No dispatch routing to do (no dispatcher attached, dispatch failed, or the decision was
    /// `ExecuteDirect`) — the caller runs the normal streaming execution path, scoped to these
    /// MCPs when non-empty (the dispatcher's narrowing hint — see `DispatchTuning::narrow_direct_tools`
    /// and `ChatSessions::build_turn_runtime`); empty means no narrowing, use the full grant.
    Proceed(Vec<String>),
    /// The turn is already answered (a clarifying question, a proposal confirmation, or a
    /// subagent's report) — this text is the final reply, no execution needed.
    Answered(String),
}

/// Render a dispatcher's clarifying questions as a plain reply.
fn format_questions(questions: &[String]) -> String {
    match questions {
        [] => "I need a bit more information before I can help with that.".to_string(),
        [only] => only.clone(),
        many => {
            let mut out = String::from("I have a few questions before I can help with that:\n");
            for q in many {
                out.push_str("- ");
                out.push_str(q);
                out.push('\n');
            }
            out
        }
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

    // ── Dispatch routing ─────────────────────────────────────────────────────

    use liberado_common::config::DispatchTuning;
    use liberado_common::{BlockReason, DispatchDecision};
    use liberado_executor::{RuntimeFactory, RuntimeSetupError};

    struct NoopFactory;
    #[async_trait]
    impl RuntimeFactory for NoopFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: liberado_common::WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            unreachable!("Clarify never builds a runtime")
        }
    }

    /// A `ChatSessions` with dispatch routing attached: `dispatch_reply` scripts the dispatcher's
    /// classifier (a `DispatchDecision` serialized as the "model" response), `chat_replies` scripts
    /// the plain conversational executor for the `ExecuteDirect` fallthrough case.
    fn sessions_with_dispatch(
        root: &std::path::Path,
        dispatch_decision: DispatchDecision,
        chat_replies: Vec<CompletionResponse>,
        orchestrator: Orchestrator,
    ) -> ChatSessions {
        let store = Arc::new(JsonlStore::new(root));
        let dispatch_provider = Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&dispatch_decision).unwrap(),
            )],
        ));
        let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

        let chat_provider = Arc::new(MockProvider::with_script("chat", chat_replies));
        let executor = Executor::new(chat_provider, liberado_executor::Budget::default());

        ChatSessions::new(store, executor, Arc::new(NoTools))
            .with_dispatch(dispatcher, Arc::new(CapabilityCatalog::new()), orchestrator)
    }

    #[tokio::test]
    async fn clarify_decision_answers_without_executing() {
        let dir = tempfile::tempdir().unwrap();
        let decision = DispatchDecision {
            action: DispatchAction::Clarify {
                questions: vec!["which vault folder do you mean?".into()],
                what_blocked: BlockReason::Ambiguous,
            },
            confidence: 0.9,
            rationale: "test".into(),
        };
        // The chat-path provider script is never touched — the turn is answered by the dispatcher
        // before any conversational execution happens.
        let orchestrator = Orchestrator::new(
            Arc::new(MockProvider::with_script("exec", Vec::new())),
            NoopFactory,
            CapabilitySet::empty(),
            Vec::new(),
            std::env::temp_dir(),
        );
        let sessions = sessions_with_dispatch(dir.path(), decision, Vec::new(), orchestrator);

        let id = sessions.create(None).await.unwrap();
        let reply = sessions.turn(id, "clean up my notes").await.unwrap();
        assert_eq!(reply, "which vault folder do you mean?");

        // Persisted like any other turn: user message + assistant reply.
        let history = sessions.history(id).await.unwrap();
        assert!(history.iter().any(|m| m.content == "clean up my notes"));
        assert!(
            history
                .iter()
                .any(|m| m.content == "which vault folder do you mean?")
        );
    }

    #[tokio::test]
    async fn execute_direct_decision_falls_through_to_normal_execution() {
        let dir = tempfile::tempdir().unwrap();
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: Vec::new(),
            },
            confidence: 0.95,
            rationale: "trivial".into(),
        };
        // ExecuteDirect never touches the orchestrator — NoopFactory would panic if it did.
        let orchestrator = Orchestrator::new(
            Arc::new(MockProvider::with_script("exec", Vec::new())),
            NoopFactory,
            CapabilitySet::empty(),
            Vec::new(),
            std::env::temp_dir(),
        );
        let sessions = sessions_with_dispatch(
            dir.path(),
            decision,
            vec![CompletionResponse::text("Hello from the normal path!")],
            orchestrator,
        );

        let id = sessions.create(None).await.unwrap();
        let reply = sessions.turn(id, "hello").await.unwrap();
        assert_eq!(reply, "Hello from the normal path!");
    }

    #[tokio::test]
    async fn propose_decision_writes_a_proposal_file_and_confirms() {
        use liberado_common::{Proposal, ProposalStatus, ProposedAction};

        let dir = tempfile::tempdir().unwrap();
        let decision = DispatchDecision {
            action: DispatchAction::Propose {
                proposed_action: ProposedAction::External {
                    description: "send the weekly report email".into(),
                },
                rationale: "sending email is external".into(),
            },
            confidence: 0.9,
            rationale: "test".into(),
        };
        // Propose never touches the factory either — NoopFactory would panic if it did.
        let orchestrator = Orchestrator::new(
            Arc::new(MockProvider::with_script("exec", Vec::new())),
            NoopFactory,
            CapabilitySet::empty(),
            Vec::new(),
            std::env::temp_dir(),
        );
        let mut sessions = sessions_with_dispatch(dir.path(), decision, Vec::new(), orchestrator);
        sessions = sessions.with_guards(Vec::new(), CapabilitySet::empty(), dir.path().join("data"));

        let id = sessions.create(None).await.unwrap();
        let reply = sessions
            .turn(id, "email the team the weekly report")
            .await
            .unwrap();

        assert!(
            reply.contains("proposal"),
            "expected a proposal confirmation, got: {reply}"
        );
        let proposals_dir = dir.path().join("data").join("proposals");
        let entries: Vec<_> = std::fs::read_dir(&proposals_dir)
            .expect("proposals dir should exist")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(entries.len(), 1, "exactly one proposal file written");
        let contents = std::fs::read_to_string(entries[0].path()).unwrap();
        let parsed = Proposal::from_note(&contents).expect("proposal note round-trips");
        assert_eq!(parsed.status, ProposalStatus::Pending);
    }

    /// A runtime offering tools from two different MCP namespaces, so narrowing between them is
    /// observable.
    struct TwoMcpTools;
    #[async_trait]
    impl ToolRuntime for TwoMcpTools {
        fn catalog(&self) -> Vec<ToolDef> {
            vec![
                ToolDef::new(
                    "tasks-mcp:add",
                    "add a task",
                    serde_json::json!({ "type": "object" }),
                ),
                ToolDef::new(
                    "email-mcp:send",
                    "send an email",
                    serde_json::json!({ "type": "object" }),
                ),
            ]
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }

    /// Build a `ChatSessions` granted both `tasks-mcp` and `email-mcp`, with dispatch attached and
    /// scripted to return `relevant_mcps`, for testing dispatcher-narrowed tool surfacing.
    fn sessions_for_narrowing_test(
        dir: &std::path::Path,
        relevant_mcps: Vec<String>,
    ) -> (ChatSessions, Arc<MockProvider>) {
        use liberado_common::Capability;

        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps,
            },
            confidence: 0.9,
            rationale: "test".into(),
        };
        let dispatch_provider = Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        ));
        let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

        let chat_provider = Arc::new(MockProvider::with_script(
            "chat",
            [CompletionResponse::text("done")],
        ));
        let executor = Executor::new(chat_provider.clone(), Budget::default());
        let store = Arc::new(JsonlStore::new(dir));
        let orchestrator = Orchestrator::new(
            Arc::new(MockProvider::with_script("exec", Vec::new())),
            NoopFactory,
            CapabilitySet::empty(),
            Vec::new(),
            std::env::temp_dir(),
        );
        let capabilities = CapabilitySet::from_iter([
            Capability::ExecuteMcp("tasks-mcp".into()),
            Capability::ExecuteMcp("email-mcp".into()),
        ]);
        let sessions = ChatSessions::new(store, executor, Arc::new(TwoMcpTools))
            .with_guards(Vec::new(), capabilities, dir.join("proposals"))
            .with_dispatch(dispatcher, Arc::new(CapabilityCatalog::new()), orchestrator);

        (sessions, chat_provider)
    }

    #[tokio::test]
    async fn execute_direct_relevant_mcps_narrows_the_surfaced_tools() {
        let dir = tempfile::tempdir().unwrap();
        let (sessions, chat_provider) =
            sessions_for_narrowing_test(dir.path(), vec!["tasks-mcp".into()]);

        let id = sessions.create(None).await.unwrap();
        sessions.turn(id, "add milk to my list").await.unwrap();

        let offered: Vec<String> = chat_provider.received_requests()[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert_eq!(
            offered,
            vec!["tasks-mcp:add".to_string()],
            "narrowed to only the relevant MCP, not the full grant"
        );
    }

    #[tokio::test]
    async fn execute_direct_empty_relevant_mcps_falls_back_to_full_grant() {
        let dir = tempfile::tempdir().unwrap();
        let (sessions, chat_provider) = sessions_for_narrowing_test(dir.path(), Vec::new());

        let id = sessions.create(None).await.unwrap();
        sessions.turn(id, "do something").await.unwrap();

        let mut offered: Vec<String> = chat_provider.received_requests()[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        offered.sort();
        assert_eq!(
            offered,
            vec!["email-mcp:send".to_string(), "tasks-mcp:add".to_string()],
            "empty relevant_mcps must fall back to the full grant"
        );
    }
}
