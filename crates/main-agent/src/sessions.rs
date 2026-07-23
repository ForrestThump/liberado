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
//! `Clarify`, `Propose`, and `DispatchSubagent` all start a hosted background session on the
//! [`GoalSessionHub`] (one-execution-engine E4) and await its terminal summary — same blocking
//! shape as the old `Orchestrator::run` path, but through the one engine. `with_dispatch` takes a
//! classifier; `with_goal_hub` is what makes non-`ExecuteDirect` (and face-agent `delegate`) work.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use liberado_common::{
    CapabilityCatalog, CapabilitySet, Consequence, DEFAULT_POOL, DispatchAction, McpDescriptor,
    ProposalSigner, WriteClass,
};
use liberado_conversation_store::{
    Author, ConversationHeader, ConversationStore, NewConversation, NewNode, StoreError, Ulid,
};
use liberado_dispatcher::{DispatchRequest, Dispatcher};
use liberado_executor::{AgentEvent, ExecError, Executor, RiskGatedToolRuntime, ToolRuntime};
use liberado_mcp::ScopedRuntime;
use liberado_provider::{Message, Role};
use liberado_session::{DomainHint, GoalSessionHub, GoalSpec, SessionGrant, SessionOrigin};
use thiserror::Error;
use tokio::sync::mpsc::Sender;

use crate::face::{DispatchBridge, FaceRuntime};
use crate::{Conversation, DEFAULT_SYSTEM_PROMPT, HUMAN_INTERFACE_SYSTEM_PROMPT};

/// Max display length for the cheap first-line default title (UTF-8 chars).
const DEFAULT_TITLE_MAX_CHARS: usize = 72;

/// What the face agent's reply collapses to when its turn deferred a decision to the human
/// out-of-band (Gap 2). The interactive proposal/permission notification is the real message;
/// this is a tiny pointer at it so the thread doesn't read as a hang.
const DEFERRED_REPLY_MARKER: &str = "⏳ waiting on your tap ↑";

/// If `deferral` was raised during the turn, replace the face agent's now-redundant reply with the
/// [`DEFERRED_REPLY_MARKER`]; otherwise return `reply` unchanged. See Gap 2 — the out-of-band
/// notification (already sent) is the sole, non-duplicated communication for that decision.
fn collapse_if_deferred(reply: String, deferral: &AtomicBool) -> String {
    if deferral.load(Ordering::Relaxed) {
        DEFERRED_REPLY_MARKER.to_string()
    } else {
        reply
    }
}

/// Cheap default conversation title: first non-empty line of `user_text`, whitespace-collapsed,
/// truncated. Does not call a model.
///
/// Callers only write this when the header title is still `None`. Agents, `PATCH`, and a future
/// `/title` slash command overwrite via [`ChatSessions::set_title`] and must not be clobbered.
pub fn default_conversation_title(user_text: &str) -> String {
    let line = user_text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.is_empty() {
        return String::new();
    }
    let collapsed: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = collapsed.chars().count();
    if count <= DEFAULT_TITLE_MAX_CHARS {
        return collapsed;
    }
    let mut out: String = collapsed
        .chars()
        .take(DEFAULT_TITLE_MAX_CHARS.saturating_sub(1))
        .collect();
    out.push('…');
    out
}

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
    /// MCP descriptors (zone declarations) for RiskGatedToolRuntime's zone-write-class gating.
    zone_catalog: Vec<McpDescriptor>,
    /// `(zone, write_class)` pairs from `Policy.zones` for the same check.
    zone_write_classes: Vec<(String, WriteClass)>,
    /// Capability grants for RiskGatedToolRuntime capability checking.
    capabilities: CapabilitySet,
    /// The vault's `proposals/` directory — a `proposals/` subdirectory under this holds proposal
    /// files (matches the daemon's own `PROPOSALS_DIR` convention, see `RiskGatedToolRuntime`'s
    /// doc comment for why this is vault-rooted, not a data-dir path).
    proposals_dir: PathBuf,
    /// Signs every proposal this session writes (dispatcher-originated and runtime-gated alike) so
    /// the daemon can detect tampering before approving one.
    signer: ProposalSigner,

    // ── Dispatch routing ─────────────────────────────────────────────────────
    /// When present, every turn is classified before execution. See the module docs.
    dispatcher: Option<Arc<Dispatcher>>,
    /// The MCP catalog the dispatcher's classifier chooses from — the same shared, live catalog
    /// the daemon's reactive path and the server's API read, snapshotted fresh per dispatch call
    /// rather than frozen at construction.
    dispatch_catalog: Arc<CapabilityCatalog>,
    /// The one execution engine — hosts non-`ExecuteDirect` pre-turn work and face-agent `delegate`.
    goals: Option<Arc<GoalSessionHub>>,
    /// Capability ceiling for the dispatcher/worker path (`policy` component `"dispatcher"`).
    /// When unset, falls back to the main-agent `capabilities` (legacy).
    dispatcher_capabilities: CapabilitySet,
    /// Face-agent mode: main agent sees `delegate` (+ optional main-agent MCP grants), not a
    /// pre-turn fleet of tools. Off by default in unit tests; production enables via config.
    delegation_mode: bool,
    /// Shared bridge for the face agent's `delegate` tool (when hub + delegation_mode).
    face_bridge: Option<Arc<DispatchBridge>>,
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
            zone_catalog: Vec::new(),
            zone_write_classes: Vec::new(),
            capabilities: CapabilitySet::empty(),
            proposals_dir: PathBuf::new(),
            signer: ProposalSigner::random(),
            dispatcher: None,
            dispatch_catalog: Arc::new(CapabilityCatalog::new()),
            goals: None,
            dispatcher_capabilities: CapabilitySet::empty(),
            delegation_mode: false,
            face_bridge: None,
        }
    }

    /// Override the system prompt written as the root node of new conversations.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Attach the goal session hub so `delegate` and non-`ExecuteDirect` pre-turn work run as
    /// hosted sessions (one-execution-engine E4). Without this, face-agent mode has no `delegate`
    /// tool and non-`ExecuteDirect` classifications fall through as plain answers about the failure.
    pub fn with_goal_hub(mut self, hub: Arc<GoalSessionHub>) -> Self {
        self.goals = Some(hub);
        self.rebuild_face_bridge();
        self
    }

    /// Enable face-agent / human-interfacer mode (built-in `delegate` tool; no pre-turn fleet).
    ///
    /// When enabled and a hub is attached, applies [`HUMAN_INTERFACE_SYSTEM_PROMPT`] unless a
    /// custom prompt was already set via [`with_system_prompt`](Self::with_system_prompt) *after*
    /// this call — prefer setting the prompt explicitly from config in the host.
    pub fn with_delegation_mode(mut self, enabled: bool) -> Self {
        self.delegation_mode = enabled;
        if enabled && self.system_prompt == DEFAULT_SYSTEM_PROMPT {
            self.system_prompt = HUMAN_INTERFACE_SYSTEM_PROMPT.to_string();
        }
        self.rebuild_face_bridge();
        self
    }

    /// Ceiling used for dispatcher classification and delegated worker sessions.
    /// Defaults to the main-agent capability set when never set.
    pub fn with_dispatcher_capabilities(mut self, caps: CapabilitySet) -> Self {
        self.dispatcher_capabilities = caps;
        self.rebuild_face_bridge();
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
    /// * `proposals_dir` - The vault's `proposals/` directory (`proposals/proposals/<id>.md` under
    ///   it holds proposal files — matches the daemon's own `PROPOSALS_DIR` convention).
    /// * `signer` - Signs every proposal this session writes.
    #[allow(clippy::too_many_arguments)]
    pub fn with_guards(
        mut self,
        consequences: Vec<(String, Consequence)>,
        capabilities: CapabilitySet,
        proposals_dir: PathBuf,
        signer: ProposalSigner,
    ) -> Self {
        self.consequences = consequences;
        self.capabilities = capabilities;
        self.proposals_dir = proposals_dir;
        self.signer = signer;
        self
    }

    /// Attach zone-write-class guard configuration (§6 #2) — optional, and separate from
    /// [`with_guards`](Self::with_guards) so existing callers don't need to change at all; a
    /// session with no zone data attached just never trips the zone-write-class check (every
    /// resolved zone would be looked up against an empty list, but `resolve_zone` itself returns
    /// `None` for every tool anyway when `zone_catalog` is empty, so this is inert, not fail-open).
    ///
    /// * `zone_catalog` - MCP descriptors (zone declarations), e.g. `catalog.descriptors()`.
    /// * `zone_write_classes` - `(zone, write_class)` pairs from `Policy.zones`.
    pub fn with_zone_guards(
        mut self,
        zone_catalog: Vec<McpDescriptor>,
        zone_write_classes: Vec<(String, WriteClass)>,
    ) -> Self {
        self.zone_catalog = zone_catalog;
        self.zone_write_classes = zone_write_classes;
        self
    }

    /// Attach pre-turn classification (see the module docs). `catalog` is the shared, live MCP
    /// catalog the classifier chooses from. Non-`ExecuteDirect` outcomes need
    /// [`with_goal_hub`](Self::with_goal_hub) to execute as hosted sessions.
    ///
    /// In **delegation mode** (`with_delegation_mode(true)`), the face agent calls `delegate`
    /// instead of receiving a pre-turn auto-answer; the hub session uses **dispatcher** capability
    /// ceilings so specialist MCPs are reachable without polluting chat tool lists.
    pub fn with_dispatch(
        mut self,
        dispatcher: Dispatcher,
        catalog: Arc<CapabilityCatalog>,
    ) -> Self {
        self.dispatcher = Some(Arc::new(dispatcher));
        self.dispatch_catalog = catalog;
        self.rebuild_face_bridge();
        self
    }

    fn rebuild_face_bridge(&mut self) {
        if !self.delegation_mode {
            self.face_bridge = None;
            return;
        }
        let Some(hub) = self.goals.clone() else {
            self.face_bridge = None;
            return;
        };
        let dispatcher_caps = if self.dispatcher_capabilities.capabilities.is_empty() {
            self.capabilities.clone()
        } else {
            self.dispatcher_capabilities.clone()
        };
        self.face_bridge = Some(Arc::new(DispatchBridge {
            hub,
            dispatcher_capabilities: dispatcher_caps,
        }));
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

    /// Fold a note into a conversation at its current leaf — the goal-session **return handoff**
    /// (session-focus S4 / D2). When a specialist session spawned from this conversation reaches a
    /// terminal state, its summary is appended here so the main agent can discuss the outcome on the
    /// next turn *without* carrying the whole specialist transcript (the context-efficiency pillar).
    /// Authored as `goal-session` (provenance) with an assistant-role body so it rehydrates as
    /// ordinary conversation context. `NotFound` if the conversation does not exist.
    pub async fn append_note(
        &self,
        conversation: Ulid,
        content: impl Into<String>,
    ) -> SessionResult<()> {
        let parent_leaf = self
            .store
            .leaf_path(conversation, None)
            .await?
            .last()
            .map(|n| n.id);
        self.store
            .append(
                conversation,
                NewNode {
                    parent_id: parent_leaf,
                    author: Author::Named("goal-session".into()),
                    message: Message::assistant(content),
                },
            )
            .await?;
        Ok(())
    }

    /// One non-streaming turn: rehydrate, run the agent over the full history, and — on success —
    /// persist the turn's new messages. A failed turn (the `?` short-circuit) persists nothing.
    ///
    /// When guard configuration is attached, the tool-advisor runs before the turn to select
    /// relevant MCPs, and the runtime is wrapped in [`RiskGatedToolRuntime`] for safety checks.
    pub async fn turn(&self, session: Ulid, user: &str) -> SessionResult<String> {
        let lock = self.session_lock(session);
        let _guard = lock.lock().await;
        self.maybe_seed_default_title(session, user).await?;
        let (mut convo, parent_leaf) = self.load(session).await?;
        let before = convo.len();

        let reply = if self.uses_face_agent() {
            let turn_deferral = Arc::new(AtomicBool::new(false));
            let turn_runtime = self.build_face_runtime(user, session, turn_deferral.clone());
            let reply = convo
                .turn(&self.executor, turn_runtime.as_ref(), user)
                .await?;
            // Gap 2: if a `delegate` this turn deferred to the human out-of-band (an interactive
            // proposal/permission notification already landed on this surface), collapse the face
            // agent's now-redundant reply to a tiny pointer at that notification.
            collapse_if_deferred(reply, &turn_deferral)
        } else {
            match self.dispatch_turn(user).await {
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
        self.maybe_seed_default_title(session, user).await?;
        let (mut convo, parent_leaf) = self.load(session).await?;
        let before = convo.len();

        if self.uses_face_agent() {
            // Streaming path (web-UI SSE): tokens are emitted live, so a post-turn deferral flag
            // can't retract an already-streamed reply — Gap 2 suppression is a buffered-`turn`
            // affordance (the Telegram surface). Pass a throwaway flag to satisfy the signature.
            let turn_deferral = Arc::new(AtomicBool::new(false));
            let turn_runtime = self.build_face_runtime(user, session, turn_deferral);
            convo
                .turn_stream(&self.executor, turn_runtime.as_ref(), user, events)
                .await?;
        } else {
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
        }
        self.persist_tail(session, &convo.history()[before..], parent_leaf)
            .await?;
        Ok(())
    }

    fn uses_face_agent(&self) -> bool {
        self.delegation_mode && self.face_bridge.is_some()
    }

    /// Face-agent runtime: built-in `delegate` is never risk-gated by MCP name (it is core).
    /// Optional `"main-agent"` MCP grants are scoped + risk-gated separately so operators can
    /// thicken the surface without exposing the fleet by default.
    ///
    /// `turn_deferral` is the per-turn flag a `delegate` raises when its subagent deferred the
    /// action to the human out-of-band — read back by [`turn`](Self::turn) to drop the redundant
    /// reply (Gap 2).
    fn build_face_runtime(
        &self,
        user: &str,
        session: Ulid,
        turn_deferral: Arc<AtomicBool>,
    ) -> Box<dyn ToolRuntime> {
        let extras = self.scoped_extras_runtime(user, session);
        Box::new(FaceRuntime::new(
            self.face_bridge.clone(),
            extras,
            Some(session.to_string()),
            turn_deferral,
        ))
    }

    /// Optional MCP tools granted to main-agent only (usually empty under the face design).
    fn scoped_extras_runtime(&self, user: &str, session: Ulid) -> Arc<dyn ToolRuntime> {
        let granted_mcps = self.capabilities.granted_mcps();
        if granted_mcps.is_empty() {
            return Arc::new(NoToolsRuntime);
        }
        let scoped: Arc<dyn ToolRuntime> =
            Arc::new(ScopedRuntime::new(self.runtime.clone(), granted_mcps));
        if self.consequences.is_empty() {
            return scoped;
        }
        Arc::new(RiskGatedToolRuntime::new(
            scoped,
            self.capabilities.clone(),
            self.consequences.clone(),
            self.zone_catalog.clone(),
            self.zone_write_classes.clone(),
            self.proposals_dir.clone(),
            user.to_string(),
            session.to_string(),
            self.signer.clone(),
            DEFAULT_POOL,
        ))
    }

    /// Every conversation header, newest first — the sidebar listing.
    ///
    /// Lazy backfill: if a header still has no title but history has a user message, persist the
    /// first-line default once so the sidebar is scannable without waiting for another turn.
    pub async fn list(&self) -> SessionResult<Vec<ConversationHeader>> {
        let mut headers = self.store.list().await?;
        for h in &mut headers {
            if h.title.is_some() {
                continue;
            }
            if let Some(title) = self.derive_default_title_from_history(h.id).await? {
                // Best-effort persist; still return the derived title for this list response.
                let _ = self.store.set_title(h.id, title.clone()).await;
                h.title = Some(title);
            }
        }
        Ok(headers)
    }

    /// The ordered message history of a session (system prompt first), for rendering a reopened
    /// conversation.
    pub async fn history(&self, session: Ulid) -> SessionResult<Vec<Message>> {
        let nodes = self.store.leaf_path(session, None).await?;
        Ok(nodes.into_iter().map(|n| n.message).collect())
    }

    /// Set the title of a conversation. Idempotent — subsequent calls overwrite the same field.
    ///
    /// Intended writers: first-line default seed, future flash-title agent, HTTP `PATCH`,
    /// future `/title` slash command. Always overwrites; never blocked by the default seed
    /// (seed only runs when the current title is `None`).
    pub async fn set_title(&self, session: Ulid, title: String) -> SessionResult<()> {
        Ok(self.store.set_title(session, title).await?)
    }

    // ── private helpers ──────────────────────────────────────────────────────

    /// If the header has no title yet, write the first-line default from `user`. Never overwrites
    /// an agent- or user-set title.
    async fn maybe_seed_default_title(&self, session: Ulid, user: &str) -> SessionResult<()> {
        let header = self.store.header(session).await?;
        if header.title.is_some() {
            return Ok(());
        }
        let title = default_conversation_title(user);
        if title.is_empty() {
            return Ok(());
        }
        self.store.set_title(session, title).await?;
        Ok(())
    }

    /// First user message → default title, or `None` if history has no usable user text.
    async fn derive_default_title_from_history(
        &self,
        session: Ulid,
    ) -> SessionResult<Option<String>> {
        let history = self.history(session).await?;
        let Some(user) = history.iter().find(|m| m.role == Role::User) else {
            return Ok(None);
        };
        let title = default_conversation_title(&user.content);
        if title.is_empty() {
            Ok(None)
        } else {
            Ok(Some(title))
        }
    }

    /// Classify `user` via the dispatcher (when attached) and resolve everything except
    /// `ExecuteDirect` — which returns [`DispatchOutcome::Proceed`] (carrying the decision's
    /// `relevant_mcps`, if any) so the caller falls through to the normal streaming execution
    /// path, scoped by whatever narrowing the dispatcher found. See the module docs for why this
    /// split exists.
    async fn dispatch_turn(&self, user: &str) -> DispatchOutcome {
        let Some(dispatcher) = &self.dispatcher else {
            return DispatchOutcome::Proceed(Vec::new()); // no dispatcher — run exactly as before
        };

        let dispatch_caps = if self.dispatcher_capabilities.capabilities.is_empty() {
            self.capabilities.clone()
        } else {
            self.dispatcher_capabilities.clone()
        };
        let req = DispatchRequest {
            goal: user.to_string(),
            // M1b: routing excludes peers marked degraded after connect/transport failure.
            catalog: self.dispatch_catalog.routing_descriptors(),
            capabilities: dispatch_caps.clone(),
            reaction_depth: 0, // user-initiated, not a background reaction
            zone_write_classes: self.zone_write_classes.clone(),
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

        // Non-ExecuteDirect: hosted session on the hub (E4). No second engine.
        let Some(hub) = &self.goals else {
            return DispatchOutcome::Answered(
                "I classified this as work that needs the dispatcher pack, but no goal hub is \
                 attached — cannot run it."
                    .into(),
            );
        };
        let correlation_id = format!("chat-{}", Ulid::new());
        let mut grant_caps = dispatch_caps;
        // Pre-turn work inside a chat turn cannot block on AskHuman (same as D-e for delegate).
        grant_caps
            .capabilities
            .retain(|c| !matches!(c, liberado_common::Capability::AskHuman));
        let goal = GoalSpec {
            id: None,
            description: user.to_string(),
            success_criteria: Vec::new(),
            domain: DomainHint::from("dispatch"),
            max_turns: 0,
            max_idle_secs: None,
            origin: Some(SessionOrigin::from_correlation(&correlation_id)),
            profile: None,
            payload: serde_json::json!({ "source": "chat-preturn" }),
        };
        let session_id = match hub
            .start_background(
                goal,
                SessionGrant {
                    capabilities: grant_caps,
                    profile: None,
                    overrides: serde_json::Value::Null,
                },
            )
            .await
        {
            Ok(id) => id,
            Err(e) => {
                return DispatchOutcome::Answered(format!(
                    "I ran into a problem starting that work: {e}"
                ));
            }
        };
        match hub.await_terminal(&session_id).await {
            Ok(snap) => {
                let summary = snap
                    .session
                    .result
                    .as_ref()
                    .map(|r| r.summary.clone())
                    .unwrap_or_else(|| "finished with no summary".into());
                DispatchOutcome::Answered(summary)
            }
            Err(e) => DispatchOutcome::Answered(format!("I ran into a problem handling that: {e}")),
        }
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
        // Chat isn't one of the daemon's named pools (it has its own separate "main-agent"
        // capability scope) — tagged "default" so an approved chat-originated proposal executes
        // via the daemon's "default" pool orchestrator on approval, exactly matching today's
        // pre-pool behavior (one orchestrator handled every approval, regardless of origin).
        Box::new(RiskGatedToolRuntime::new(
            inner,
            self.capabilities.clone(),
            self.consequences.clone(),
            self.zone_catalog.clone(),
            self.zone_write_classes.clone(),
            self.proposals_dir.clone(),
            user.to_string(),
            session.to_string(),
            self.signer.clone(),
            DEFAULT_POOL,
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
#[path = "sessions/tests.rs"]
mod tests;
