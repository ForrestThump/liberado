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
    Capability, CapabilityCatalog, CapabilitySet, Consequence, DEFAULT_POOL, DispatchAction,
    McpDescriptor, ProposalSigner, WriteClass, mcp_of,
};
use liberado_conversation_store::{
    Author, ConversationHeader, ConversationStore, MessageNode, NewConversation, NewNode,
    StoreError, Ulid,
};
use liberado_dispatcher::{DispatchRequest, Dispatcher};
use liberado_executor::{AgentEvent, ExecError, Executor, RiskGatedToolRuntime, ToolRuntime};
use liberado_mcp::ScopedRuntime;
use liberado_provider::{Message, Provider, Role};
use liberado_session::{DomainHint, GoalSessionHub, GoalSpec, SessionGrant, SessionOrigin};
use thiserror::Error;
use tokio::sync::mpsc::Sender;

use crate::compaction::{self, COMPACTION_AUTHOR, COMPACTION_TAIL_AUTHOR, CompactionConfig};
use crate::face::{DispatchBridge, FaceRuntime};
use crate::{Conversation, DEFAULT_SYSTEM_PROMPT, HUMAN_INTERFACE_SYSTEM_PROMPT};

/// Max display length for the cheap first-line default title (UTF-8 chars).
const DEFAULT_TITLE_MAX_CHARS: usize = 72;

/// [`Author::Named`] identity of the note recording a session-profile switch.
///
/// Named rather than `Author::System` because a `system` node in this store is the face agent's
/// prompt, and every reader drops those — the WebUI's history filter most visibly. A switch the
/// human cannot see in the thread is not meaningfully recorded.
pub const PROFILE_AUTHOR: &str = "profile";

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

/// What one turn runs under, resolved from the session's profile (or the daemon's defaults).
///
/// A struct rather than three lookups so a profile switch cannot land between them — a turn running
/// one profile's tools under another's delegation setting would be a genuinely confusing bug to
/// chase, and the cost of preventing it is one type.
struct TurnSettings {
    capabilities: CapabilitySet,
    /// Whether the face agent may `delegate`. Resolved: the profile's setting, else the daemon's.
    delegation: bool,
    /// Extra system-prompt text for this profile, injected per turn.
    prompt_append: Option<String>,
    /// The profile's name, for logging. `None` is "no profile named", which is distinct from a
    /// profile that grants nothing — the distinction this plan keeps insisting on.
    profile: Option<String>,
    /// Model for this session's turns. `None` = the daemon's configured model.
    ///
    /// Resolved here with everything else for the reason this struct exists: a profile switch must
    /// not land between two reads and give a turn one profile's model under another's tools.
    model: Option<String>,
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
    /// Live catalog for hot-reload-safe consequence/zone lookups (preferred over snapshots).
    live_catalog: Option<Arc<CapabilityCatalog>>,
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
    /// Automatic context compaction (CH3) — config + the provider that writes summaries.
    /// `None` = never compact (tests, and hosts that never wired it).
    compaction: Option<CompactionEngine>,
}

/// The moving parts of automatic compaction: the tunables, plus the provider used for the one
/// summarization completion per compaction (the chat face's own provider in production — see
/// `docs/roadmap/context-compaction-plan.md` for why no dedicated summarizer model yet).
///
/// `trigger_tokens` is held under a mutex so face-model hot-swap can re-resolve the threshold
/// without rebuilding `ChatSessions` (process-wide `POST /api/models/select` / Telegram `/model`).
struct CompactionEngine {
    config: CompactionConfig,
    /// Live absolute trigger; starts as `config.trigger_tokens`, updated by
    /// [`ChatSessions::set_compaction_trigger_tokens`].
    trigger_tokens: std::sync::Mutex<u32>,
    provider: Arc<dyn Provider>,
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
            live_catalog: None,
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
            compaction: None,
        }
    }

    /// Enable automatic context compaction (CH3) with `config`; `provider` runs the one
    /// summarization completion per compaction. Per turn, when the estimated size of history +
    /// the incoming message exceeds `config.trigger_tokens`, everything older than the last
    /// `keep_recent_turns` user turns is rolled into a persisted summary marker
    /// ([`COMPACTION_AUTHOR`]) and the model-visible history resumes from it. A failed summary
    /// never fails the turn — it runs uncompacted instead.
    pub fn with_compaction(
        mut self,
        config: CompactionConfig,
        provider: Arc<dyn Provider>,
    ) -> Self {
        let trigger_tokens = std::sync::Mutex::new(config.trigger_tokens);
        self.compaction = Some(CompactionEngine {
            config,
            trigger_tokens,
            provider,
        });
        self
    }

    /// Update the live compaction threshold (absolute estimated tokens) after a face-model
    /// hot-swap. No-op when compaction was never wired. Concurrent with turns: the next
    /// `maybe_compact` observes the new value.
    pub fn set_compaction_trigger_tokens(&self, tokens: u32) {
        if let Some(engine) = &self.compaction {
            *engine
                .trigger_tokens
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = tokens;
            tracing::info!(
                trigger_tokens = tokens,
                "chat compaction: trigger_tokens updated for face model change"
            );
        }
    }

    /// Current live compaction threshold, if compaction is enabled on this host.
    pub fn compaction_trigger_tokens(&self) -> Option<u32> {
        self.compaction.as_ref().map(|engine| {
            *engine
                .trigger_tokens
                .lock()
                .unwrap_or_else(|p| p.into_inner())
        })
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

    /// Prefer live catalog lookups in per-turn RiskGated gates (topology MCP hot-reload).
    pub fn with_live_catalog(mut self, catalog: Arc<CapabilityCatalog>) -> Self {
        self.live_catalog = Some(catalog);
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
        self.create_conversation(title, false, SessionGrant::default())
            .await
    }

    /// Create a conversation running under an already-resolved session profile.
    ///
    /// The grant arrives resolved: turning a profile *name* into capabilities is
    /// `Config::resolve_session_profile`'s job, and it fails closed on an unknown or disabled name,
    /// so a typo can never reach here as "no profile" (which would silently mean the default grant).
    pub async fn create_with_grant(
        &self,
        title: Option<String>,
        grant: SessionGrant,
    ) -> SessionResult<Ulid> {
        self.create_conversation(title, false, grant).await
    }

    /// Create an **incognito** conversation: RAM only, never written to disk, never listed.
    ///
    /// Its own method rather than a `bool` on [`create`](Self::create) because ephemerality is not a
    /// tuning knob — it is a promise to the person typing, and a call site should have to say the
    /// word. (It also leaves the thirty-odd existing `create(None)` calls saying exactly what they
    /// already meant.)
    ///
    /// Note what this does *not* cover: the turn's **side effects**. A tool the agent calls during an
    /// incognito chat still writes what it writes — a vault note, a memory, an audit entry. What is
    /// ephemeral is the transcript, not the consequences.
    pub async fn create_incognito(&self, title: Option<String>) -> SessionResult<Ulid> {
        self.create_conversation(title, true, SessionGrant::default())
            .await
    }

    async fn create_conversation(
        &self,
        title: Option<String>,
        ephemeral: bool,
        grant: SessionGrant,
    ) -> SessionResult<Ulid> {
        let header = self
            .store
            .create(NewConversation {
                title,
                parent_conversation: None,
                spawned_by: None,
                ephemeral,
                grant,
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

    /// Switch a conversation onto a different session profile, and record that it happened.
    ///
    /// Two writes, both needed for different reasons:
    ///
    /// 1. **The header** carries the new grant, which is what the next turn reads. Append-only, so
    ///    the previous header lines remain as the record of what ran before.
    /// 2. **A transcript node**, so the switch is visible where the conversation is read and findable
    ///    by `chat-search`. The header is authoritative but invisible — a change of authority the
    ///    human cannot see in the thread is not meaningfully "recorded".
    ///
    /// Takes effect on the **next** turn: the tool runtime is rebuilt per turn from this grant, so an
    /// in-flight turn finishes under the authority it started with rather than changing mid-flight.
    ///
    /// **Human-only by construction, not by check.** This is reachable from a surface endpoint and is
    /// deliberately not registered as a tool anywhere, so no agent can re-authorise its own session.
    /// If you are about to expose it to a model, that is the decision to revisit — not this method.
    pub async fn set_profile(&self, conversation: Ulid, grant: SessionGrant) -> SessionResult<()> {
        let label = grant.profile.clone();
        self.store.set_grant(conversation, grant).await?;

        // `Author::Named`, not `Author::System`: a `system` node in this store is the face agent's
        // prompt, and every reader (the WebUI's history filter, compaction) drops those. A named
        // author keeps the note visible and says who wrote it.
        let parent_leaf = self
            .store
            .leaf_path(conversation, None)
            .await?
            .last()
            .map(|n| n.id);
        let note = match label {
            Some(name) => format!("Session profile: {name}"),
            None => "Session profile cleared — using the default grant.".to_string(),
        };
        self.store
            .append(
                conversation,
                NewNode {
                    parent_id: parent_leaf,
                    author: Author::Named(PROFILE_AUTHOR.into()),
                    message: Message::system(note),
                },
            )
            .await?;
        Ok(())
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
        let (nodes, parent_leaf) = self.load(session).await?;
        let (mut convo, parent_leaf) = self.maybe_compact(session, nodes, parent_leaf, user).await;
        let before = convo.len();

        // Read per turn, not cached: a profile switch mid-conversation takes effect on the next
        // turn, which is the whole of "switchable" — no restart, no reconnect.
        let settings = self.turn_settings(session).await;
        // A profile's model applies to this turn only. Specialised per turn rather than set on the
        // shared provider, so one chat choosing a model cannot change it for every other session.
        let executor = self.executor.with_model(settings.model.clone());
        // Resolved once: the prompt the model reads and the tools it is handed must agree, and
        // deciding that twice is how they came apart in the first place.
        let face_agent = self.uses_face_agent(settings.delegation);
        if !face_agent {
            convo.apply_direct_agent_prompt();
        }
        convo.apply_prompt_append(settings.prompt_append.as_deref());

        let reply = if face_agent {
            let turn_deferral = Arc::new(AtomicBool::new(false));
            let turn_runtime = self.build_face_runtime(
                user,
                session,
                settings.capabilities.clone(),
                turn_deferral.clone(),
            );
            // Derived from the runtime the executor is about to be handed, never from a list built
            // beside it — see `Conversation::apply_available_tools`.
            self.state_tool_surface(&mut convo, session, &settings, turn_runtime.as_ref());
            let reply = convo.turn(&executor, turn_runtime.as_ref(), user).await?;
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
                    let turn_runtime = self.build_turn_runtime(
                        user,
                        session,
                        &relevant_mcps,
                        &settings.capabilities,
                    );
                    self.state_tool_surface(&mut convo, session, &settings, turn_runtime.as_ref());
                    convo.turn(&executor, turn_runtime.as_ref(), user).await?
                }
            }
        };
        self.persist_tail(session, convo.turn_tail(before), parent_leaf)
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
        let (nodes, parent_leaf) = self.load(session).await?;
        let (mut convo, parent_leaf) = self.maybe_compact(session, nodes, parent_leaf, user).await;
        let before = convo.len();

        let settings = self.turn_settings(session).await;
        let executor = self.executor.with_model(settings.model.clone());
        let face_agent = self.uses_face_agent(settings.delegation);
        if !face_agent {
            convo.apply_direct_agent_prompt();
        }
        convo.apply_prompt_append(settings.prompt_append.as_deref());

        if face_agent {
            // Streaming path (web-UI SSE): tokens are emitted live, so a post-turn deferral flag
            // can't retract an already-streamed reply — Gap 2 suppression is a buffered-`turn`
            // affordance (the Telegram surface). Pass a throwaway flag to satisfy the signature.
            let turn_deferral = Arc::new(AtomicBool::new(false));
            // `settings.capabilities`, not a second lookup: `TurnSettings` exists so a profile
            // switch cannot land between two reads of the same turn's authority, and this path was
            // quietly doing exactly that.
            let turn_runtime = self.build_face_runtime(
                user,
                session,
                settings.capabilities.clone(),
                turn_deferral,
            );
            self.state_tool_surface(&mut convo, session, &settings, turn_runtime.as_ref());
            convo
                .turn_stream(&executor, turn_runtime.as_ref(), user, events)
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
                    let turn_runtime = self.build_turn_runtime(
                        user,
                        session,
                        &relevant_mcps,
                        &settings.capabilities,
                    );
                    self.state_tool_surface(&mut convo, session, &settings, turn_runtime.as_ref());
                    convo
                        .turn_stream(&executor, turn_runtime.as_ref(), user, events)
                        .await?;
                }
            }
        }
        self.persist_tail(session, convo.turn_tail(before), parent_leaf)
            .await?;
        Ok(())
    }

    /// Whether this turn runs as the face agent (delegating) rather than driving the dispatcher
    /// directly.
    ///
    /// `delegation` is the **session's** resolved setting, not the process-wide flag: turning
    /// dispatch off is most of what a "basic chat" profile means, and a profile that could only
    /// shorten the tool list would leave the agent still handing work to the dispatcher.
    ///
    /// `face_bridge` still gates it. A daemon with no hub attached has no `delegate` tool to offer,
    /// so a profile asking for delegation there gets the direct path rather than a tool that does
    /// not exist.
    fn uses_face_agent(&self, delegation: bool) -> bool {
        delegation && self.face_bridge.is_some()
    }

    /// The capability set a turn in `session` runs under.
    ///
    /// **A session with no profile gets the process-wide grant**, which is every conversation that
    /// existed before profiles and every one started without naming one. Falling back to the
    /// session's empty default instead would have silently stripped the tools from every existing
    /// chat the moment this shipped — a migration disguised as a feature.
    ///
    /// A *named* profile is authoritative even when its capability set is empty, so "this chat may
    /// call nothing" stays sayable and distinct from "nothing was chosen". The name is what carries
    /// the intent; an empty `capabilities` alone cannot.
    ///
    /// A session the store cannot produce a header for also falls back — a lookup failure must not
    /// quietly become an authority change.
    /// Test-only since the streaming path stopped re-reading the grant it had already resolved:
    /// production reads authority exactly once per turn, through [`TurnSettings`]. Kept because four
    /// resolution tests assert on it directly, and they are the tests that pin "no profile means the
    /// process grant, an empty named profile means nothing".
    #[cfg(test)]
    async fn session_capabilities(&self, session: Ulid) -> CapabilitySet {
        self.turn_settings(session).await.capabilities
    }

    /// Everything a turn in `session` runs under, resolved once per turn.
    ///
    /// One lookup rather than three: capabilities, delegation and the prompt nudge all come from the
    /// same header read, and reading them separately would let a profile switch land *between* them —
    /// a turn running one profile's tools under another's delegation setting.
    async fn turn_settings(&self, session: Ulid) -> TurnSettings {
        let header = match self.store.header(session).await {
            Ok(header) => header,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    session = %session,
                    "could not read session grant; falling back to the process-wide defaults"
                );
                return self.default_turn_settings();
            }
        };
        // A *named* profile is authoritative; the absence of one means the daemon's defaults. See
        // `ConversationHeader::grant` for why the name, not an empty capability set, is the signal.
        if header.grant.profile.is_none() {
            return self.default_turn_settings();
        }
        TurnSettings {
            capabilities: header.grant.capabilities,
            delegation: header.grant.delegation.unwrap_or(self.delegation_mode),
            prompt_append: header.grant.prompt_append,
            profile: header.grant.profile,
            model: header.grant.model,
        }
    }

    /// Log the tool surface this turn actually holds, then state it to the model.
    ///
    /// Both from one `catalog()` call on the runtime being handed to the executor, so the operator's
    /// log line and the model's prompt can never describe different tool sets. Reading it twice
    /// would reintroduce, in the diagnostics, exactly the drift the manifest exists to remove.
    ///
    /// At INFO because this is the line whose absence cost three debugging rounds: `tool surface
    /// ready` fires once at boot with the daemon default, so nothing recorded what any individual
    /// session was offered, and every profile failure had to be diagnosed through the store and the
    /// source instead of the log.
    fn state_tool_surface(
        &self,
        convo: &mut Conversation,
        session: Ulid,
        settings: &TurnSettings,
        runtime: &dyn ToolRuntime,
    ) {
        let catalog = runtime.catalog();
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
        tracing::info!(
            session = %session,
            profile = settings.profile.as_deref().unwrap_or("(none)"),
            delegation = settings.delegation,
            count = names.len(),
            tools = ?names,
            "chat turn: tool surface"
        );
        convo.apply_available_tools(&catalog);
    }

    /// The daemon-wide delegation default, for tests asserting the inherit path without hard-coding
    /// what that default happens to be.
    #[cfg(test)]
    pub(crate) fn delegation_mode_for_test(&self) -> bool {
        self.delegation_mode
    }

    fn default_turn_settings(&self) -> TurnSettings {
        TurnSettings {
            capabilities: self.capabilities.clone(),
            delegation: self.delegation_mode,
            prompt_append: None,
            profile: None,
            model: None,
        }
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
        capabilities: CapabilitySet,
        turn_deferral: Arc<AtomicBool>,
    ) -> Box<dyn ToolRuntime> {
        let extras = self.scoped_extras_runtime(user, session, capabilities);
        Box::new(FaceRuntime::new(
            self.face_bridge.clone(),
            extras,
            Some(session.to_string()),
            turn_deferral,
        ))
    }

    /// Whether runtime risk/zone/consequence gates must wrap tool calls.
    ///
    /// A boot-time empty consequence snapshot is **not** enough to skip gating when a live
    /// catalog is attached — empty→add hot-reload can register write peers after construction.
    fn risk_gate_enabled(&self) -> bool {
        self.live_catalog.is_some()
            || !self.consequences.is_empty()
            || !self.zone_catalog.is_empty()
    }

    /// The MCP tools this **session** may call directly.
    ///
    /// `capabilities` is the session's own grant — its profile — not the process-wide
    /// `self.capabilities`. That is the whole of per-session tool scoping: the runtime was already
    /// rebuilt per turn and already handed the session id, so the only thing missing was asking the
    /// session what it is allowed to do.
    ///
    /// Built with `from_capabilities`, which enforces per **tool** and fails closed. The MCP-name
    /// constructor could not express a partial grant, and its empty-list-means-everything default
    /// would turn a restrictive profile into an unrestricted one.
    fn scoped_extras_runtime(
        &self,
        user: &str,
        session: Ulid,
        capabilities: CapabilitySet,
    ) -> Arc<dyn ToolRuntime> {
        if capabilities.granted_mcps().is_empty() && capabilities.granted_tools().is_empty() {
            return Arc::new(NoToolsRuntime);
        }
        let scoped: Arc<dyn ToolRuntime> = Arc::new(ScopedRuntime::from_capabilities(
            self.runtime.clone(),
            capabilities.clone(),
        ));
        // Never return raw scoped tools when a live catalog (or snapshot gate data) is configured:
        // empty boot-time `consequences` must not bypass Write(zone)/consequence after reload.
        if !self.risk_gate_enabled() {
            return scoped;
        }
        let mut gated = RiskGatedToolRuntime::new(
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
        );
        if let Some(cat) = &self.live_catalog {
            gated = gated.with_live_catalog(cat.clone());
        }
        Arc::new(gated)
    }

    /// Every conversation header, newest first — the sidebar listing.
    ///
    /// Lazy backfill: if a header still has no title but history has a user message, persist the
    /// first-line default once so the sidebar is scannable without waiting for another turn.
    /// Permanently delete a conversation. Thin passthrough: the store owns the semantics (see
    /// [`ConversationStore::delete`]) and there is no cached conversation state here to invalidate —
    /// every turn rehydrates from the store.
    pub async fn delete(&self, id: Ulid) -> SessionResult<()> {
        self.store.delete(id).await?;
        Ok(())
    }

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
    ///
    /// Shows the **full** transcript — compaction never deletes, so everything a compaction elided
    /// from the model's view is still here, marker included. The one thing dropped is compaction's
    /// re-appended tail *copies* ([`COMPACTION_TAIL_AUTHOR`]): their originals are already on the
    /// log before the marker, so rendering both would repeat the last `keep_recent_turns` turns
    /// after every compaction.
    pub async fn history(&self, session: Ulid) -> SessionResult<Vec<Message>> {
        let nodes = self.store.leaf_path(session, None).await?;
        Ok(nodes
            .into_iter()
            .filter(|n| !n.author.is_compaction_tail_copy())
            .map(|n| n.message)
            .collect())
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
                    ..Default::default()
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
    /// When no guard configuration is attached (and no live catalog), returns the raw
    /// `self.runtime` unchanged. If a live catalog is wired, never pass through unscoped tools —
    /// empty grants mean no tools (not every peer on the live registry).
    fn build_turn_runtime(
        &self,
        user: &str,
        session: Ulid,
        relevant_mcps: &[String],
        capabilities: &CapabilitySet,
    ) -> Box<dyn ToolRuntime> {
        // Deliberately still `self.capabilities`: this asks "is this daemon guarded at all", which is
        // a property of the *process*, not of one session. Reading the session's set here would let a
        // profile that legitimately grants nothing ("this chat may call nothing") fall into the
        // unguarded fixture branch and be handed every tool — a fail-open in the one case that most
        // needs to fail closed.
        if self.capabilities.capabilities.is_empty()
            && self.consequences.is_empty()
            && self.live_catalog.is_none()
        {
            // Truly unguarded test fixtures — raw runtime only when no live catalog is attached.
            return Box::new(PassThroughRuntime(self.runtime.clone()));
        }

        // Capability scoping: surface only what *this session* is granted, every turn, regardless of
        // how the message is phrased. The model sees the full granted tool set (robust — no missed
        // requests).
        //
        // Scoped by **capability, not by MCP name**. `granted_mcps()` reports only `ExecuteMcp`, so
        // an `ExecuteTool("turbovault:tasks_list")` grant contributed nothing to an MCP-level
        // allow-list and a per-tool profile surfaced no tools at all. `from_capabilities` is the
        // constructor step 1 added for exactly this — "the only one that can express a partial grant
        // over a single MCP" — and this path had never adopted it.
        //
        // And the session's set, not the process's. The face path has always passed the session
        // grant explicitly; this one read `self.capabilities`, so a profile resolved into the
        // header, showed up over the API, and then surfaced as nothing — on precisely the path its
        // own `delegation = false` selects. An unprofiled session resolves to the process grant, so
        // this is unchanged for every chat that names no profile.
        //
        // Dispatcher narrowing (the token-efficiency piece — see module docs) is applied *within*
        // the grant and can only shrink it: a capability survives only if the goal's relevant MCPs
        // mention its server. Non-execute capabilities pass through untouched — they say nothing
        // about which tools to surface, and dropping them here would quietly narrow the risk gate.
        let scope: CapabilitySet = if relevant_mcps.is_empty() {
            capabilities.clone()
        } else {
            CapabilitySet::from_iter(
                capabilities
                    .capabilities
                    .iter()
                    .filter(|c| match c {
                        Capability::ExecuteMcp(name) => relevant_mcps.iter().any(|r| r == name),
                        Capability::ExecuteTool(qualified) => {
                            relevant_mcps.iter().any(|r| r == mcp_of(qualified))
                        }
                        _ => true,
                    })
                    .cloned(),
            )
        };
        tracing::debug!(tools = ?scope.granted_tools(), "chat turn tool scope");
        // Fails closed on an empty set by construction, so no no-tools special case is needed:
        // `ScopedRuntime::new`'s empty allow-list means pass-through, which is the wrong sense for a
        // grant and was previously guarded by hand.
        let inner: Arc<dyn ToolRuntime> = Arc::new(ScopedRuntime::from_capabilities(
            self.runtime.clone(),
            scope,
        ));

        // Wrap in RiskGatedToolRuntime for safety guards (capability / consequence / magnitude).
        // Chat isn't one of the daemon's named pools (it has its own separate "main-agent"
        // capability scope) — tagged "default" so an approved chat-originated proposal executes
        // via the daemon's "default" pool orchestrator on approval, exactly matching today's
        // pre-pool behavior (one orchestrator handled every approval, regardless of origin).
        let mut gated = RiskGatedToolRuntime::new(
            inner,
            // The same set that scoped the surface above. Gating on the process grant while scoping
            // on the session's would surface a profile's tool and then refuse the call.
            capabilities.clone(),
            self.consequences.clone(),
            self.zone_catalog.clone(),
            self.zone_write_classes.clone(),
            self.proposals_dir.clone(),
            user.to_string(),
            session.to_string(),
            self.signer.clone(),
            DEFAULT_POOL,
        );
        if let Some(cat) = &self.live_catalog {
            gated = gated.with_live_catalog(cat.clone());
        }
        Box::new(gated)
    }

    /// Get-or-insert the per-session turn lock, so two turns on the same conversation serialize
    /// (and never interleave their appends) while different conversations run concurrently.
    fn session_lock(&self, session: Ulid) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks
            .entry(session)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Rehydrate a session's current leaf path as the **model-visible** node view, alongside the
    /// id of the leaf node — the parent the turn's first new message will hang off of.
    ///
    /// Applies the compaction elision rule: everything strictly between the system root and the
    /// *latest* [`COMPACTION_AUTHOR`] marker is dropped from the view (it stays on disk — rendered
    /// history via [`history`](Self::history) and `chat-search` see the full transcript). The leaf
    /// id is unaffected: elision only drops middle nodes, never the suffix.
    async fn load(&self, session: Ulid) -> SessionResult<(Vec<MessageNode>, Option<Ulid>)> {
        let nodes = self.store.leaf_path(session, None).await?;
        let parent_leaf = nodes.last().map(|n| n.id);
        Ok((elide_before_latest_marker(nodes), parent_leaf))
    }

    /// Compact `nodes` if warranted, returning the [`Conversation`] to run the turn over and the
    /// parent the turn's first new message hangs off of. Unchanged inputs pass through untouched.
    ///
    /// The full sequence (estimation, boundary selection, summarization, persistence) is described
    /// in `docs/roadmap/context-compaction-plan.md`. Every failure mode degrades to *running
    /// uncompacted* — a missing summary must never cost the human their turn.
    async fn maybe_compact(
        &self,
        session: Ulid,
        nodes: Vec<MessageNode>,
        parent_leaf: Option<Ulid>,
        incoming_user: &str,
    ) -> (Conversation, Option<Ulid>) {
        let messages: Vec<Message> = nodes.iter().map(|n| n.message.clone()).collect();
        let pass_through = || (Conversation::from_history(messages.clone()), parent_leaf);

        let Some(engine) = &self.compaction else {
            return pass_through();
        };
        if !engine.config.enabled {
            return pass_through();
        }

        let trigger_tokens = *engine
            .trigger_tokens
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let estimate = compaction::estimate_tokens(&messages)
            + compaction::estimate_tokens(&[Message::user(incoming_user)]);
        if estimate <= trigger_tokens {
            return pass_through();
        }

        let Some(boundary) =
            compaction::elision_boundary(&messages, engine.config.keep_recent_turns)
        else {
            // Over the trigger but nothing elidable (e.g. one enormous turn): compaction can't
            // help — proceed and let the provider/budget surface the real limit.
            tracing::debug!(
                estimate,
                trigger = trigger_tokens,
                "chat compaction: over trigger but nothing elidable"
            );
            return pass_through();
        };

        let elided: Vec<Message> = messages[1..boundary].to_vec(); // skip the system root at 0
        let tail: &[MessageNode] = &nodes[boundary..];
        let request = compaction::summary_request(&elided, &engine.config);
        let summary = match engine.provider.complete(request).await {
            Ok(resp) => match resp.content.filter(|c| !c.trim().is_empty()) {
                Some(text) => text,
                None => {
                    tracing::warn!(
                        "chat compaction: summarizer returned empty — running uncompacted"
                    );
                    return pass_through();
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "chat compaction: summarizer failed — running uncompacted");
                return pass_through();
            }
        };

        // Persist the compacted view: marker off the pre-compaction leaf, then the tail re-appended
        // verbatim (original authors/content, fresh ids) so the view is a contiguous log suffix —
        // see the plan doc's "persisted-marker model" for why the tail is copied, not referenced.
        let marker = compaction::marker_message(&summary);
        let mut parent = match self
            .store
            .append(
                session,
                NewNode {
                    parent_id: parent_leaf,
                    author: Author::Named(COMPACTION_AUTHOR.into()),
                    message: marker.clone(),
                },
            )
            .await
        {
            Ok(node) => Some(node.id),
            Err(e) => {
                tracing::warn!(error = %e, "chat compaction: marker append failed — running uncompacted");
                return pass_through();
            }
        };
        // The marker is already durable. Re-append the kept tail best-effort so the compacted
        // view is a contiguous log suffix. Two invariants on partial failure:
        //
        // 1. **This turn's model view is always complete.** We push every tail message into
        //    `view` regardless of whether its append succeeded — a store blip must never strip
        //    the kept tail from the conversation the model is about to run.
        // 2. **Do not stop re-appending after the first failure.** Parent stays at the last
        //    successful node, so later successes still form a linear chain. Breaking early used
        //    to permanently drop the rest of the suffix on every subsequent load (elision hides
        //    the pre-marker originals).
        //
        // Without a transactional multi-node append, a durable half-written suffix remains
        // possible; operators see the error log. The next load then resumes from marker +
        // whatever re-appended.
        //
        // The copies are authored [`COMPACTION_TAIL_AUTHOR`], not the original author: the
        // originals are still on the log before the marker, and stamping the copies is what lets
        // raw-leaf-path readers (rendered history, `Author::User` turn indexing, search) show each
        // message once. The model view reads `message`, never `author`, so it is unaffected.
        let mut view: Vec<Message> = vec![messages[0].clone(), marker];
        let mut tail_persist_failures: usize = 0;
        for tail_node in tail {
            match self
                .store
                .append(
                    session,
                    NewNode {
                        parent_id: parent,
                        author: Author::Named(COMPACTION_TAIL_AUTHOR.into()),
                        message: tail_node.message.clone(),
                    },
                )
                .await
            {
                Ok(node) => parent = Some(node.id),
                Err(e) => {
                    tail_persist_failures += 1;
                    tracing::error!(
                        error = %e,
                        failures = tail_persist_failures,
                        "chat compaction: tail re-append failed — keeping full in-memory tail for this turn and continuing remaining appends"
                    );
                }
            }
            view.push(tail_node.message.clone());
        }
        if tail_persist_failures > 0 {
            tracing::error!(
                failures = tail_persist_failures,
                tail_messages = tail.len(),
                "chat compaction: incomplete persisted tail after marker write; this turn's view is complete but the next load may miss unpersisted tail messages"
            );
        }

        tracing::info!(
            estimate_before = estimate,
            elided_messages = elided.len(),
            tail_messages = tail.len(),
            "chat context compacted"
        );
        (Conversation::from_history(view), parent)
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

/// The model-visible view of a leaf path: the system root plus everything from the **latest**
/// compaction marker onward. Everything strictly between them was summarized into that marker and
/// would otherwise be paid for, in tokens, on every single turn forever. Identity when no marker
/// exists. The marker node itself is kept — it carries the rolling summary.
///
/// Root handling: the path's first node is the conversation root (the system prompt `create`
/// wrote). A marker at index 0 is impossible (the root is authored `System`, never
/// [`COMPACTION_AUTHOR`]), so keeping `nodes[0]` never keeps a marker by accident.
fn elide_before_latest_marker(nodes: Vec<MessageNode>) -> Vec<MessageNode> {
    let marker = nodes
        .iter()
        .rposition(|n| matches!(&n.author, Author::Named(name) if name == COMPACTION_AUTHOR));
    match marker {
        Some(c) if c > 0 => {
            let mut view = Vec::with_capacity(nodes.len() - c + 1);
            view.push(nodes[0].clone());
            view.extend_from_slice(&nodes[c..]);
            view
        }
        _ => nodes,
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
