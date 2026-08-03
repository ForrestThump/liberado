use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use liberado_common::Event;
use liberado_daemon::Reaction;
use liberado_executor::ToolRuntime;
use liberado_main_agent::ChatSessions;
use liberado_provider::{ToolDef, ToolInvocation};
use tokio::sync::{Mutex, mpsc::UnboundedSender};

use liberado_bootstrap::{Config, LiveMcpController};
use liberado_common::{CapabilityCatalog, CapabilitySet};
use liberado_provider::Provider;
use liberado_session::GoalSessionHub;

use chat_client_contract::{ReactionEvent, ReactionOutcome};

use crate::hooks::{IdempotencyCache, ResolvedHook};
use crate::shutdown::DrainGate;

pub struct AppState {
    pub start_time: Instant,
    pub reactions: Arc<Mutex<Vec<ReactionEvent>>>,
    pub dispatcher_attached: bool,
    pub orchestrator_attached: bool,
    pub vault_path: String,
    /// Domain-neutral goal sessions (coding + life packs). Surfaces are clients of this hub.
    pub goals: Arc<GoalSessionHub>,
    /// Present when `DEEPSEEK_API_KEY` is set — the durable, session-keyed chat agent. All
    /// persistence orchestration lives inside [`ChatSessions`]; the HTTP handlers are thin adapters.
    pub chat: Option<Arc<ChatSessions>>,
    /// Number of connected chat tools (0 when MCP is unconfigured or failed to connect).
    pub chat_tools: usize,
    /// Names of the tools available to chat, for diagnostic visibility.
    pub chat_tool_names: Vec<String>,
    /// Live capability catalog describing every registered MCP server's name, description,
    /// and consequence. Populated at boot from `config.topology.mcps` and updated at runtime
    /// as MCPs come and go. Exposed via `GET /api/catalog`.
    pub catalog: Arc<CapabilityCatalog>,
    /// Root directory of the converged session store (`liberado_config::sessions_dir()`) — used by
    /// `GET /api/conversations/search`, which scans the JSONL logs directly rather than going
    /// through the store (read-only, and it wants no part of the store's write locks).
    ///
    /// It was `conversations_root`, pointing at `<data_dir>/conversations`. The name outlived the
    /// thing: there is one directory now, holding every session.
    pub sessions_root: PathBuf,
    /// The data dir the daemon was started with (`liberado_config::data_dir()`), held rather than
    /// re-resolved so handlers that read journals under it are testable without mutating process
    /// environment — `data_dir()` reads `LIBERADO_DATA_DIR` on every call, and `set_var` across
    /// parallel tests is both racy and `unsafe`. `GET /api/status` reads the latency journal here.
    pub data_dir: PathBuf,
    /// The `"main-agent"` component's capability grant (`policy.toml`) — which MCPs chat's own
    /// tool surface may call directly. `GET /api/catalog` uses this (via `.grants_mcp`) to label
    /// each MCP's `visible_to_main_agent` flag, independently of `dispatcher_capabilities` below.
    pub main_agent_capabilities: CapabilitySet,
    /// The `"dispatcher"` component's capability grant — which MCPs the daemon's
    /// dispatch/orchestrate pipeline may call. See `main_agent_capabilities` above.
    pub dispatcher_capabilities: CapabilitySet,
    /// The converged `Session` store (S5′) — the *same* object `chat` and `goals` are built over.
    /// Held directly so `GET /api/sessions` can serve the one unified list, instead of a client
    /// polling two endpoints and stitching chats and goal sessions together itself.
    pub sessions: Arc<liberado_session_store::SessionStore>,
    /// The resolved config — held so `POST /api/goals` can turn a session's `profile` into its
    /// [`SessionGrant`](liberado_session::SessionGrant) (S6) via `Config::resolve_session_profile`.
    /// The *server* owns this resolution because the session kernel must stay free of the config
    /// stack: the kernel is handed an already-resolved authority, never a config key to look up.
    pub config: Arc<Config>,
    /// Boot-time model id snapshot. Prefer [`Provider::model`] via `state.provider` for the live
    /// hot-swapped face model (`POST /api/models/select`).
    pub model_name: Option<String>,
    /// Shared inference backend — used by `GET /api/models` to call `Provider::list_models`.
    /// `None` when no provider is configured.
    pub provider: Option<Arc<dyn Provider>>,
    /// Resolved external webhook hooks (`crates/server/src/hooks.rs`), keyed by name — only
    /// enabled hooks whose secret was actually resolvable from the environment. Empty when
    /// `topology.hooks` is empty (the endpoint 404s every request in that case, same as today
    /// with no hooks configured).
    pub hooks: HashMap<String, ResolvedHook>,
    /// Where `POST /api/hooks/{name}` pushes an accepted trigger — a clone of the same channel
    /// every other `EventSource` (vault-watch, cron) pushes onto, from `Daemon::event_sender()`.
    pub hook_tx: UnboundedSender<Event>,
    /// Best-effort dedup for a hook's `X-Liberado-Idempotency-Key`, shared across all hooks (keys
    /// are already namespaced by hook name — see `hooks::trigger_hook`).
    pub hook_idempotency: IdempotencyCache,
    /// Shared MCP peer controller (catalog + registry). `POST /api/mcp/reload` re-applies the
    /// hand-edited topology MCP slice without process restart.
    pub live_mcp: LiveMcpController,
    /// Graceful-shutdown drain gate: when not accepting, turn-starting routes refuse with
    /// `shutting_down` (see `crate::shutdown`). Attach and other read paths stay open.
    pub drain: DrainGate,
}

/// Build the kernel [`liberado_main_agent::CompactionConfig`] from topology: absolute triggers
/// resolved **per declared model** (and the live face slug), plus a daemon-default for chats with
/// no model of their own.
///
/// Call once at chat boot (`lib.rs`); face hot-swap only updates the default via
/// [`resync_compaction_trigger_for_face_model`].
pub fn compaction_config_for_face(
    config: &Config,
    face_model: &str,
) -> liberado_main_agent::CompactionConfig {
    let compact = &config.topology.main_agent.compaction;
    let models = &config.topology.models;
    let default_trigger = compact.resolve_trigger_tokens(Some(face_model), models);
    let mut model_trigger_tokens = std::collections::HashMap::new();
    for m in models {
        model_trigger_tokens.insert(
            m.name.clone(),
            compact.resolve_trigger_tokens(Some(m.name.as_str()), models),
        );
    }
    // Face slug may not be a declared [[models]] entry (free-form provider model).
    model_trigger_tokens
        .entry(face_model.to_string())
        .or_insert(default_trigger);
    // Same absolute `resolve_trigger_tokens` would return for an undeclared conversation model.
    let unknown_model_trigger_tokens =
        compact.resolve_trigger_tokens(Some("__liberado_unknown_model__"), models);
    liberado_main_agent::CompactionConfig {
        enabled: compact.enabled,
        trigger_tokens: default_trigger,
        model_trigger_tokens,
        unknown_model_trigger_tokens,
        keep_recent_turns: compact.keep_recent_turns as usize,
        summary_max_tokens: compact.summary_max_tokens,
        tool_result_max_chars: compact.tool_result_max_chars as usize,
    }
}

/// Re-resolve the **daemon-default** chat compaction threshold after a face-model hot-swap.
///
/// Uses config-tier `CompactionSettings::resolve_trigger_tokens` (per-model pct / absolute).
/// Only conversations with no model of their own observe the new value — per-conversation
/// model thresholds stay fixed in the trigger table wired at boot (the shared-number bug was
/// that this path retuned every chat).
pub fn resync_compaction_trigger_for_face_model(state: &AppState, face_model: &str) {
    let Some(chat) = state.chat.as_ref() else {
        return;
    };
    let compact = &state.config.topology.main_agent.compaction;
    let tokens = compact.resolve_trigger_tokens(Some(face_model), &state.config.topology.models);
    chat.set_compaction_trigger_tokens(tokens);
}

/// A tool runtime with no tools — chat still works (just conversation) when no MCP is configured.
#[allow(dead_code)]
pub struct NoTools;

#[async_trait::async_trait]
impl ToolRuntime for NoTools {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Err("no tools are configured".into())
    }
}

impl AppState {
    pub fn reaction_tx(&self) -> UnboundedSender<Reaction> {
        let reactions = self.reactions.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Reaction>();

        tokio::spawn(async move {
            while let Some(reaction) = rx.recv().await {
                // Mirror the old `liberado <vault>` stderr line so `liberado serve` still surfaces
                // reactions to operators, not just to the `/api/reactions` buffer below.
                tracing::info!(
                    event_type = %reaction.event.event_type,
                    path = reaction.event.payload.path.as_deref().unwrap_or_default(),
                    correlation_id = %reaction.event.correlation_id,
                    outcome = reaction.outcome.label(),
                    "REACTION"
                );

                // Map the daemon's ReactionOutcome enum variants directly to the wire enum.
                // The granular label() strings ("(observed)", "acted:reported", etc.) are kept
                // in the tracing line above; only the wire outcome is the 3-variant enum.
                let wire_outcome = match &reaction.outcome {
                    liberado_daemon::ReactionOutcome::Observed => ReactionOutcome::Observed,
                    liberado_daemon::ReactionOutcome::Decided(_) => ReactionOutcome::Decided,
                    liberado_daemon::ReactionOutcome::Acted(_) => ReactionOutcome::Acted,
                    liberado_daemon::ReactionOutcome::Dispatched { session_id } => {
                        ReactionOutcome::Dispatched {
                            session_id: session_id.clone(),
                        }
                    }
                };

                let event = ReactionEvent {
                    event_type: reaction.event.event_type.clone(),
                    timestamp: reaction.event.timestamp.to_rfc3339(),
                    source: reaction.event.source.clone(),
                    correlation_id: reaction.event.correlation_id.clone(),
                    path: reaction.event.payload.path.clone(),
                    outcome: wire_outcome,
                };
                let mut guard = reactions.lock().await;
                guard.push(event);
                if guard.len() > 500 {
                    let excess = guard.len() - 500;
                    guard.drain(..excess);
                }
            }
        });

        tx
    }
}

#[cfg(test)]
impl AppState {
    /// A minimal `AppState` for handler tests: a real store and a real `ChatSessions`, everything
    /// else inert.
    ///
    /// The store and chat are genuine on purpose — a test that stubs the store cannot prove
    /// anything about whether a request destroys data, which is the only reason this exists.
    pub(crate) fn for_test(
        sessions: Arc<liberado_session_store::SessionStore>,
        chat: Option<Arc<ChatSessions>>,
        root: PathBuf,
    ) -> Self {
        let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
        // Leaked so the receiver never drops: a closed channel would make `hook_tx.send` fail in a
        // way no test here is about.
        std::mem::forget(_hook_rx);
        Self {
            start_time: Instant::now(),
            reactions: Arc::new(Mutex::new(Vec::new())),
            dispatcher_attached: false,
            orchestrator_attached: false,
            vault_path: root.join("vault").to_string_lossy().into_owned(),
            goals: Arc::new(GoalSessionHub::new(
                liberado_session_store::SessionStore::clone(&sessions),
            )),
            chat,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(CapabilityCatalog::new()),
            data_dir: root.clone(),
            sessions_root: root,
            main_agent_capabilities: CapabilitySet::empty(),
            dispatcher_capabilities: CapabilitySet::empty(),
            sessions,
            config: Arc::new(Config::default()),
            model_name: None,
            provider: None,
            hooks: HashMap::new(),
            hook_tx,
            hook_idempotency: IdempotencyCache::default(),
            live_mcp: LiveMcpController::empty(),
            drain: DrainGate::default(),
        }
    }
}
