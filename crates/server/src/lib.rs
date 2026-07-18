//! # liberado-server
//!
//! The Liberado daemon process, as a library: it assembles the provider/chat/daemon (via
//! `liberado_bootstrap`), spawns the vault watch loop, and serves the HTTP/SSE API (`docs/reference/api.md`)
//! plus the built web frontend. The `liberado serve` subcommand calls [`run`]; this crate ships no
//! binary and does not init the global tracing subscriber (the CLI owns that, so a library embedding
//! us doesn't fight over it).

mod api;
mod cron_delivery;
mod hooks;
mod state;
mod sticky;
mod telegram;

use std::sync::Arc;
use std::time::{Duration, Instant};

use std::path::Path;

use axum::Router;
use liberado_common::{CapabilityCatalog, CapabilitySet, WriteProvenance};
use liberado_daemon::Daemon;
use liberado_dispatcher::Dispatcher;
use liberado_executor::{Budget, Executor, ToolRuntime};
use liberado_main_agent::ChatSessions;
use liberado_mcp::McpRegistry;

use liberado_provider::Provider;
use liberado_session_store::SessionStore;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::{info, warn};

/// Where `dx build --release --package liberado-webui --web` places the built frontend.
const DIST_DIR: &str = "target/dx/liberado-webui/release/web/public";

use crate::state::{AppState, NoTools};

const DEFAULT_PORT: u16 = 4201;

/// Run the daemon over `vault_path`: build the shared provider/MCP, assemble the chat agent and the
/// vault daemon (watch loop), then serve the HTTP/SSE API and static frontend until killed. This is
/// the daemon's foreground entry point — `liberado serve` calls it. The caller is expected to have
/// already initialised the tracing subscriber.
pub async fn run(vault_path: String) -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("LIBERADO_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    // Load + validate the config up front (Decision 14 fail-fast). A bad config is a hard error
    // here rather than a half-booted daemon; the message names the file/setting to fix.
    let (config, _) = liberado_bootstrap::load_config(liberado_bootstrap::config_dir().as_deref())?;

    // Resolve the vault path CLI-over-config: the `run` argument wins (the CLI always supplies one);
    // an empty argument falls back to `topology.vault_path`. Both empty is a hard error.
    let vault_path = if vault_path.trim().is_empty() {
        let from_config = config.topology.vault_path.to_string_lossy().into_owned();
        if from_config.trim().is_empty() {
            return Err(
                "no vault path: pass one to `liberado serve` or set topology.vault_path".into(),
            );
        }
        from_config
    } else {
        vault_path
    };

    // One live catalog, shared by every consumer (the API, the daemon's reactive dispatch, chat's
    // own dispatch) — built once here instead of each independently snapshotting `topology.mcps`.
    let capability_catalog = Arc::new(liberado_bootstrap::capability_catalog_from_config(&config));

    // Build the provider once and share it between the daemon (dispatch/execute) and chat.
    let provider = liberado_bootstrap::provider_from_config(&config);
    let dispatcher_attached = provider.is_some();
    let model_name = provider.as_ref().map(|p| p.model().to_string());
    let mcp = liberado_bootstrap::mcp_registry_from_config(&config);
    let orchestrator_attached = dispatcher_attached && mcp.is_some();

    let guidance = dispatcher_guidance_source(&vault_path).await;

    // ── The converged Session store (S5′ / D7) ──────────────────────────────────────────────
    // ONE store, under `<data_dir>/sessions/`, handed to *both* chat and the goal-session hub.
    // A chat and a goal session are the same record with a different `goal: Option` — so they share
    // an id space, a directory, and a log format. Chat sees it through `ConversationStore`, the
    // kernel through `SessionRecordStore`; neither knows the other is there.
    //
    // The previous `<data_dir>/conversations/` and `<data_dir>/goal-sessions/` directories are left
    // untouched but no longer read (deliberate: fresh start, nothing destroyed).
    //
    // One `sessions_dir()`, not a `.join("sessions")` here and another in the `chat-search` MCP —
    // that is precisely how the MCP got left behind pointing at the dead `conversations/` directory.
    let sessions_root = liberado_bootstrap::sessions_dir();
    let sessions = Arc::new(liberado_session_store::SessionStore::open(&sessions_root).await);

    // Goal session hub first — the **one** execution engine (one-execution-engine plan E3/E4).
    // Life-ops demo always; coding when a provider is available; dispatch pack so cron/webhook/
    // delegate are hosted sessions, not a second engine. Built before chat so `delegate` can use it.
    let mut goals_hub = liberado_session::GoalSessionHub::new(SessionStore::clone(&sessions));
    goals_hub.register_pack(Arc::new(liberado_session::LifeOpsDemoRunner));
    if let Some(p) = provider.as_ref() {
        let work_parent = liberado_bootstrap::data_dir().join("goal-workspaces");
        let _ = std::fs::create_dir_all(&work_parent);
        goals_hub.register_pack(Arc::new(liberado_coder_agent::CodingSessionPack::new(
            p.clone(),
            work_parent,
        )));
    }
    if let Some(pack) = liberado_bootstrap::build_dispatch_pack(
        provider.as_ref(),
        &config,
        capability_catalog.clone(),
        Path::new(&vault_path),
        guidance.clone(),
    ) {
        goals_hub.register_pack(Arc::new(pack));
        info!("goal session packs: life + coding + dispatch");
    } else if provider.is_some() {
        info!("goal session packs: life + coding (no dispatch pack)");
    } else {
        info!("goal session packs: life only (no provider)");
    }
    // E5: when a session awaits input and nobody has the stream open, ping out-of-band.
    if let Some(n) = liberado_notify::TelegramNotifier::from_env() {
        goals_hub = goals_hub.with_alert(Arc::new(NotifySessionAlert(Arc::new(n))));
        info!("session alerts: telegram notifier attached");
    }
    let goals = Arc::new(goals_hub);

    let (chat, chat_tools, chat_tool_names) = build_chat(
        provider.clone(),
        mcp,
        &config,
        capability_catalog.clone(),
        Path::new(&vault_path),
        guidance.clone(),
        SessionEngine {
            store: sessions.clone(),
            goals: goals.clone(),
        },
    )
    .await;

    let daemon = Daemon::open("webui", &vault_path).await?;
    let daemon = liberado_bootstrap::configure_daemon(
        daemon,
        provider.as_ref(),
        &config,
        capability_catalog.clone(),
        Path::new(&vault_path),
        guidance,
    )
    // Every reaction is a hosted background session on the hub (E3) — joinable, cancellable.
    .with_goal_hub(goals.clone());

    // Shared state for chat-aware cron delivery (`docs/ideas/cron-delivery-timing-idea.md`): the
    // sticky Telegram session id (also owned by the chat bridge) and the "last human message" clock
    // (also stamped by the approval bot). Built here so the daemon's delivery notifier and the
    // bot/bridge below all point at the same instances.
    //
    // The sticky id is now **persisted** across restarts (`<data_dir>/telegram-sticky-session`, on the
    // same volume as the session store): on boot we restore the last conversation so a container
    // restart no longer forces an implicit `/new`. A restored id is adopted only if the conversation
    // still exists (validated against the chat store) — a stale pointer is dropped, not resurrected.
    let telegram_sticky = if let Some(chat_sessions) = chat.as_ref() {
        let cs = chat_sessions.clone();
        sticky::StickySession::load(
            liberado_bootstrap::data_dir().join("telegram-sticky-session"),
            move |id| async move {
                cs.list()
                    .await
                    .map(|headers| headers.iter().any(|h| h.id == id))
                    .unwrap_or(false)
            },
        )
        .await
    } else {
        sticky::StickySession::ephemeral()
    };
    let telegram_activity: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    // When both Telegram and a chat surface exist, route the daemon's cron delivery through the
    // chat-delivering notifier: it folds each brief into the sticky conversation and defers the send
    // around active chat. Proposals still go out immediately (its inner notifier). Without chat or
    // Telegram, the daemon keeps whatever `configure_daemon` set (plain immediate notify).
    let daemon = match (chat.as_ref(), liberado_notify::TelegramNotifier::from_env()) {
        (Some(chat_sessions), Some(inner)) => {
            let cdn = cron_delivery::ChatDeliveringNotifier::new(
                Arc::new(inner),
                chat_sessions.clone(),
                telegram_sticky.clone(),
                telegram_activity.clone(),
                Duration::from_secs(config.tuning.cron_delivery.quiet_delay_secs),
                Duration::from_secs(config.tuning.cron_delivery.deliver_by_secs),
            );
            info!("cron delivery: folding briefs into the sticky Telegram chat (quiet-delay defer)");
            daemon.with_notifier(Arc::new(cdn))
        }
        _ => daemon,
    };

    // The webhook hooks endpoint's seam into the daemon's reactive pipeline — a clone of the same
    // channel every `EventSource` (vault-watch, cron) pushes onto. Grabbed before `daemon` moves
    // into its own spawned task below, same pattern as the vault/signer clones just above it.
    let hook_tx = daemon.event_sender();
    let resolved_hooks = hooks::resolve_hooks(&config.topology);
    info!(hooks = resolved_hooks.len(), "webhook hooks resolved");

    // For `GET /api/catalog`'s per-MCP visibility labeling — the same two component names
    // `build_chat`/`configure_daemon` already resolve grants for (see their own
    // `capabilities_for("main-agent")`/`capabilities_for("dispatcher")` calls below).
    let main_agent_capabilities = config.policy.capabilities_for("main-agent");
    let dispatcher_capabilities = config.policy.capabilities_for("dispatcher");

    let state = Arc::new(AppState {
        start_time: Instant::now(),
        reactions: Arc::new(Mutex::new(Vec::new())),
        dispatcher_attached,
        orchestrator_attached,
        vault_path: vault_path.clone(),
        goals,
        chat,
        chat_tools,
        chat_tool_names,
        catalog: capability_catalog,
        sessions: sessions.clone(),
        // Search scans the converged log directly, so it now reads every session's file rather than
        // a chat-only directory. In practice it still only *finds* chat turns: search matches
        // message nodes, and packs currently record their transcripts as events. A pack that wrote
        // its turns as nodes would become searchable for free — no change here.
        sessions_root: sessions_root.clone(),
        main_agent_capabilities,
        dispatcher_capabilities,
        config: Arc::new(config.clone()),
        model_name,
        provider: provider.clone(),
        hooks: resolved_hooks,
        hook_tx,
        hook_idempotency: Default::default(),
    });

    // Optional — Telegram bot: proposal Approve/Reject/Revise buttons + free-form chat when a
    // ChatSessions surface exists. Only when a provider is attached and LIBERADO_TELEGRAM_* env
    // vars are set. Cloning the vault/signer here, before `daemon` moves into its own spawn below,
    // gives the bot its own handle onto the same vault (`Vault` is cheap to clone).
    if let Some(p) = provider.as_ref()
        && let Some(mut bot) = liberado_telegram_approvals::ApprovalBot::from_env(
            daemon.vault().clone(),
            daemon.signer().clone(),
            p.clone(),
            config.tuning.telegram_approvals.clone(),
        )
    {
        if state.chat.is_some() {
            bot = bot
                .with_chat(Arc::new(crate::telegram::TelegramChatBridge {
                    state: state.clone(),
                    session_id: telegram_sticky.clone(),
                }))
                .with_activity_tracker(telegram_activity.clone());
            info!("Telegram free-form chat surface attached (slash commands enabled)");
        }
        tokio::spawn(bot.run());
    }

    let reaction_tx = state.reaction_tx();
    let daemon_handle = tokio::spawn(async move {
        daemon.run(reaction_tx).await.ok();
    });

    let app = Router::new()
        .route("/api/status", axum::routing::get(api::status))
        .route("/api/models", axum::routing::get(api::models))
        .route("/api/models/select", axum::routing::post(api::select_model))
        .route("/api/catalog", axum::routing::get(api::catalog))
        .route("/api/reactions", axum::routing::get(api::reactions))
        .route("/api/vault", axum::routing::get(api::vault))
        .route("/api/chat", axum::routing::post(api::chat))
        .route(
            "/api/chat/stream",
            axum::routing::get(api::chat_stream_get).post(api::chat_stream_post),
        )
        .route(
            "/api/conversations",
            axum::routing::get(api::list_conversations),
        )
        .route(
            "/api/conversations/search",
            axum::routing::get(api::search_conversations),
        )
        .route(
            "/api/conversations/{id}",
            axum::routing::get(api::get_conversation).patch(api::patch_conversation_title),
        )
        .route(
            "/api/hooks/{name}",
            axum::routing::post(hooks::trigger_hook),
        )
        // The one unified list (S5′): chats and goal sessions, same rows.
        .route("/api/sessions", axum::routing::get(api::sessions_list))
        // Branch a conversation, keeping the original (copy semantics — a snapshot, not a pointer).
        .route(
            "/api/sessions/{id}/fork",
            axum::routing::post(api::session_fork),
        )
        .route("/api/goals/domains", axum::routing::get(api::goals_domains))
        .route(
            "/api/goals",
            axum::routing::get(api::goals_list).post(api::goals_start),
        )
        .route("/api/goals/{id}", axum::routing::get(api::goals_get))
        .route(
            "/api/goals/{id}/stream",
            axum::routing::get(api::goals_stream),
        )
        .route(
            "/api/goals/{id}/cancel",
            axum::routing::post(api::goals_cancel),
        )
        .route(
            "/api/goals/{id}/message",
            axum::routing::post(api::goals_message),
        )
        .layer(CorsLayer::permissive())
        .with_state(state)
        .fallback_service(ServeDir::new(DIST_DIR));

    let addr = format!("0.0.0.0:{port}");
    info!("Web UI server listening on http://{}", addr);
    info!("API endpoints:");
    info!("  GET /api/status  — daemon status");
    info!("  GET /api/models  — live provider model catalog");
    info!("  POST /api/models/select  — hot-swap active model");
    info!("  GET /api/reactions?limit=20  — recent reactions");
    info!("  GET /api/vault  — vault info");
    info!("  GET|POST /api/goals  — list / start goal sessions (coding + life packs)");
    info!("  GET /api/goals/{{id}}/stream  — SSE goal session events");
    info!("  /  — static frontend (build with `dx build` from crates/webui/)");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    daemon_handle.abort();
    Ok(())
}

/// Load + validate the config and print a concise summary — the `liberado config check` subcommand.
/// On success, prints the config dir used and the policy/topology surface (zones, grants, MCPs,
/// vault path) to stdout. On failure, prints the actionable error to stderr and returns it, so an
/// operator editing config over `ssh` gets an exit code and a fixable message (not a panic).
///
/// `dir = None` resolves the default config dir (the CLI passes `None`); an explicit dir is honoured
/// for tests / overrides.
pub fn config_check(dir: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = dir
        .map(Path::to_path_buf)
        .or_else(liberado_bootstrap::config_dir);
    match liberado_bootstrap::load_config(resolved.as_deref()) {
        Ok((config, provenance)) => {
            let where_ = resolved
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none — all defaults)".to_string());
            println!("config OK");
            println!("  config dir: {where_}");
            println!("  sources:");
            println!(
                "    topology:  {}",
                provenance.topology.as_deref().unwrap_or("(default)")
            );
            println!(
                "    policy:    {}",
                provenance.policy.as_deref().unwrap_or("(default)")
            );
            println!(
                "    tuning:    {}",
                provenance.tuning.as_deref().unwrap_or("(default)")
            );
            let top_src = provenance.topology.as_deref().unwrap_or("(default)");
            let pol_src = provenance.policy.as_deref().unwrap_or("(default)");
            println!(
                "  vault_path: {}    [{top_src}]",
                config.topology.vault_path.display()
            );
            println!(
                "  zones:      {}                  [{pol_src}]",
                config.policy.zones.len()
            );
            println!(
                "  grants:     {}                  [{pol_src}]",
                config.policy.grants.len()
            );
            println!(
                "  mcps:        {}                  [{top_src}]",
                config.topology.mcps.len()
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("config check failed: {e}");
            Err(Box::new(e))
        }
    }
}

/// Build the chat agent when a provider is available: a connected tool runtime (the configured MCP
/// server, or none) + the executor + the **converged session store** (S5′), injected by the caller.
/// Returns `(chat, tool_count, tool_names)` so callers can surface tool availability in diagnostics.
/// Returns `(None, 0, empty)` when there's no provider.
///
/// The store is a parameter rather than built here because [`run`] builds **one** store and hands
/// the same object to chat *and* to the goal-session hub — that shared object is what convergence
/// actually is (D7). `ChatSessions` still only sees the `ConversationStore` trait, so it neither
/// knows nor cares that goal sessions live in the same log.
///
/// `catalog` is the shared, live `CapabilityCatalog` built once in [`run`] — the same object the
/// daemon's reactive dispatch and the server's own `/api/catalog` read, not an independent snapshot.
///
/// When guard configuration (catalog, capabilities) is present, ChatSessions is configured with
/// the tool-advisor and RiskGatedToolRuntime for every turn.
/// The one execution engine, as the chat face needs it: the converged store (chat lens) and the hub
/// that runs every goal session. They are always wired together — a chat turn that delegates hands
/// the goal to `goals` and its transcript lands in `store` — so they travel as one thing.
struct SessionEngine {
    /// The **concrete** converged store (chat lens).
    store: Arc<SessionStore>,
    /// Goal sessions — including `delegate` — run here, not in a second inline orchestrator.
    goals: Arc<liberado_session::GoalSessionHub>,
}

async fn build_chat(
    provider: Option<Arc<dyn Provider>>,
    mcp: Option<McpRegistry>,
    config: &liberado_bootstrap::Config,
    catalog: Arc<CapabilityCatalog>,
    vault_path: &Path,
    guidance: Option<Arc<dyn liberado_common::ToolGuidanceSource>>,
    engine: SessionEngine,
) -> (Option<Arc<ChatSessions>>, usize, Vec<String>) {
    let SessionEngine { store, goals } = engine;
    let provider = match provider {
        Some(p) => p,
        None => return (None, 0, Vec::new()),
    };

    // Main-agent = chat face surface (usually thin). Dispatcher = worker ceiling for delegated tools.
    let main_agent_caps = config.policy.capabilities_for("main-agent");
    let dispatcher_caps = config.policy.capabilities_for("dispatcher");
    let main_agent_cfg = &config.topology.main_agent;

    // Consequence catalog + proposals dir: shared with `configure_daemon`'s own Orchestrator wiring
    // (`liberado_bootstrap::guard_context`) so the two boot paths can't independently drift on how
    // either is derived. A one-time snapshot at boot is fine here — MCP declarations aren't
    // runtime-dynamic yet — but the dispatch-routing catalog below stays the live `Arc` so it and the
    // daemon/API never drift apart from each other.
    let guard = liberado_bootstrap::guard_context(&catalog, &config.policy, vault_path);
    let catalog_is_empty = guard.consequences.is_empty();

    // Face-agent tool surface: optional main-agent MCP grants only. Specialist work goes through
    // `delegate` → hub → dispatch pack (dispatcher caps), not the face agent's own tool list.
    let runtime = connect_chat_runtime(&provider, mcp, &main_agent_caps, &guard).await;

    let mut tool_names: Vec<String> = runtime.catalog().iter().map(|t| t.name.clone()).collect();
    // Face-agent surface is usually just `delegate` (+ optional main-agent MCP grants).
    if main_agent_cfg.delegation_mode {
        tool_names = vec![liberado_main_agent::DELEGATE_TOOL_NAME.to_string()];
        let granted = main_agent_caps.granted_mcps();
        if !granted.is_empty() {
            tool_names.extend(runtime.catalog().iter().filter_map(|t| {
                let mcp = t.name.split_once(':').map(|(m, _)| m).unwrap_or(&t.name);
                if granted.iter().any(|g| g == mcp) {
                    Some(t.name.clone())
                } else {
                    None
                }
            }));
        }
    }
    let tool_count = tool_names.len();
    if tool_count > 0 {
        info!(
            count = tool_count,
            tools = ?tool_names,
            delegation_mode = main_agent_cfg.delegation_mode,
            "chat: tool surface ready"
        );
    } else {
        info!("chat: no tools available — the model can only converse, not act");
    }

    // ── Build the guarded ChatSessions ───────────────────────────────────────
    let consequence_count = guard.consequences.len();
    let system_prompt = main_agent_cfg.system_prompt.clone().unwrap_or_else(|| {
        if main_agent_cfg.delegation_mode {
            liberado_main_agent::HUMAN_INTERFACE_SYSTEM_PROMPT.to_string()
        } else {
            liberado_main_agent::DEFAULT_SYSTEM_PROMPT.to_string()
        }
    });

    let mut sessions = ChatSessions::new(
        store,
        Executor::new(provider.clone(), Budget::default()),
        runtime,
    )
    .with_system_prompt(system_prompt)
    .with_goal_hub(goals)
    .with_guards(
        guard.consequences,
        main_agent_caps,
        guard.proposals_dir,
        guard.signer.clone(),
    )
    .with_zone_guards(guard.zone_catalog, guard.zone_write_classes)
    .with_dispatcher_capabilities(dispatcher_caps)
    .with_delegation_mode(main_agent_cfg.delegation_mode);

    if !catalog_is_empty {
        info!(
            count = consequence_count,
            "chat: runtime safety guards enabled (capability-scoped tools + RiskGatedToolRuntime)"
        );
    }

    // Pre-turn classification (legacy mode) + face-agent `delegate` needs a dispatcher for the
    // classifier; execution is always the hub's dispatch pack.
    let mut dispatcher = Dispatcher::new(
        provider,
        config.tuning.dispatch.clone(),
        config.tuning.concurrency.max_reaction_depth,
    );
    if let Some(g) = guidance {
        dispatcher = dispatcher.with_guidance(g);
    }
    if main_agent_cfg.delegation_mode {
        info!("chat: face-agent mode — human interfacer + delegate tool (hub hosts work)");
    } else {
        info!(
            "chat: legacy dispatch mode (pre-turn routing + main-agent MCP tools on stream path)"
        );
    }
    sessions = sessions.with_dispatch(dispatcher, catalog);

    (Some(Arc::new(sessions)), tool_count, tool_names)
}

/// Build the dispatcher's optional procedural-memory guidance source (`liberado-dispatch-logic-spec.md`
/// §2 steps 1/5), opting in only when `LIBERADO_DISPATCHER_GUIDANCE=1` is set. Off by default:
/// building one means opening a vault-backed store and loading an embedding model in the daemon
/// process itself — the same store `liberado-memory-mcp` (a separate subprocess) already exposes
/// to agents, so an unopted-in deployment isn't paying for a second copy of that model just to run
/// `liberado serve`. Any failure (bad vault path, model load error) degrades to `None` — this is
/// an optimization, never something worth failing boot over.
async fn dispatcher_guidance_source(
    vault_path: &str,
) -> Option<Arc<dyn liberado_common::ToolGuidanceSource>> {
    if std::env::var("LIBERADO_DISPATCHER_GUIDANCE").as_deref() != Ok("1") {
        return None;
    }

    let vault = match liberado_vault::Vault::open("dispatcher-guidance", vault_path).await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "dispatcher guidance: failed to open vault — continuing without it");
            return None;
        }
    };

    let model =
        std::env::var("LIBERADO_MEMORY_MODEL").unwrap_or_else(|_| "bge-small-en-v1.5".to_string());
    let embedder: Arc<dyn turbovault_vector::EmbeddingEngine> =
        match turbovault_vector::FastembedEngine::new(&model, None) {
            Ok(e) => Arc::new(e),
            Err(e) => {
                warn!(error = %e, "dispatcher guidance: failed to load embedding model — continuing without it");
                return None;
            }
        };

    match liberado_memory_store::MemoryStore::open(
        vault,
        "memory/procedural",
        embedder,
        None,
        liberado_memory_store::MemoryStoreConfig::default(),
    )
    .await
    {
        Ok(store) => {
            info!("dispatcher guidance: procedural memory enabled");
            Some(Arc::new(store))
        }
        Err(e) => {
            warn!(error = %e, "dispatcher guidance: failed to open procedural memory store — continuing without it");
            None
        }
    }
}

/// Connect chat's tool runtime once, reused for its lifetime. Specialist work goes through the
/// hub's dispatch pack (`delegate` / pre-turn non-ExecuteDirect); this runtime is only the face
/// agent's optional direct MCP grants.
async fn connect_chat_runtime(
    _provider: &Arc<dyn Provider>,
    mcp: Option<McpRegistry>,
    _capabilities: &CapabilitySet,
    _guard: &liberado_bootstrap::GuardContext,
) -> Arc<dyn ToolRuntime> {
    match mcp {
        Some(registry) => {
            let provenance = WriteProvenance::agent("liberado-chat", "chat-session");
            let (rt, failed) = registry.connect_all_best_effort(provenance).await;
            let rt: Arc<dyn ToolRuntime> = Arc::from(rt);
            if failed.is_empty() {
                info!("chat: connected MCP tools");
            } else {
                warn!(failed = ?failed, "chat: some MCPs failed to connect — continuing with the rest");
            }
            // The registry is only used for the face agent's optional direct tools; worker
            // execution lives in the dispatch pack on the hub (E4). Dropping the registry here
            // is fine — connect_all already produced the ToolRuntime.
            drop(registry);
            rt
        }
        None => {
            info!("chat: no MCP configured — chat will be conversation-only");
            Arc::new(NoTools)
        }
    }
}

/// Bridges `liberado_notify::Notifier` into the hub's [`SessionAlert`](liberado_session::SessionAlert)
/// port so an unwatched awaiting session pings Telegram (E5).
struct NotifySessionAlert(Arc<dyn liberado_notify::Notifier>);

#[async_trait::async_trait]
impl liberado_session::SessionAlert for NotifySessionAlert {
    async fn session_needs_you(&self, session_id: &str, prompt: &str) {
        let message = format!(
            "Liberado: a session needs your input.\n\
             session: {session_id}\n\
             {prompt}\n\
             Answer in the TUI or via POST /api/goals/{session_id}/message"
        );
        match self.0.notify(&message).await {
            // Logged on *success*, not only on failure. Whether a ping fired is a real behavioural
            // claim — the hub suppresses it when someone is already watching the session — and until
            // this line existed the only way to check it was to look at a human's phone.
            Ok(()) => tracing::info!(%session_id, "session alert sent — nobody was watching"),
            Err(e) => tracing::warn!(error = %e, %session_id, "session alert notification failed"),
        }
    }
}
