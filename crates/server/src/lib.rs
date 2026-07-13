//! # liberado-server
//!
//! The Liberado daemon process, as a library: it assembles the provider/chat/daemon (via
//! `liberado_bootstrap`), spawns the vault watch loop, and serves the HTTP/SSE API (`docs/reference/api.md`)
//! plus the built web frontend. The `liberado serve` subcommand calls [`run`]; this crate ships no
//! binary and does not init the global tracing subscriber (the CLI owns that, so a library embedding
//! us doesn't fight over it).

mod api;
mod hooks;
mod state;

use std::sync::Arc;
use std::time::Instant;

use std::path::Path;

use axum::Router;
use liberado_common::{CapabilityCatalog, CapabilitySet, DEFAULT_POOL, WriteProvenance};
use liberado_daemon::Daemon;
use liberado_dispatcher::Dispatcher;
use liberado_executor::{Budget, Executor, ToolRuntime};
use liberado_main_agent::ChatSessions;
use liberado_mcp::McpRegistry;
use liberado_orchestrator::Orchestrator;
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
    let sessions_root = liberado_bootstrap::data_dir().join("sessions");
    let sessions = Arc::new(liberado_session_store::SessionStore::open(&sessions_root).await);

    let (chat, chat_tools, chat_tool_names) = build_chat(
        provider.clone(),
        mcp,
        &config,
        capability_catalog.clone(),
        Path::new(&vault_path),
        guidance.clone(),
        sessions.clone(),
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
    // Every reaction the daemon takes — a cron firing, a webhook, an external vault edit — is now
    // recorded as a **background session** in the same store chat and `/spawn` use (S5′ step 5).
    // They were unattended *and* invisible; only the first half of that was ever intended.
    .with_session_store(sessions.clone());

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

    // Goal session hub: life-ops demo always; coding when a provider is available. It runs on the
    // **same** store as chat (S5′) — `SessionStore` implements the kernel's `SessionRecordStore`,
    // so goal sessions rehydrate on boot from the very directory chat conversations live in.
    let mut goals_hub = liberado_session::GoalSessionHub::new(SessionStore::clone(&sessions));
    goals_hub.register_pack(Arc::new(liberado_session::LifeOpsDemoRunner));
    if let Some(p) = provider.as_ref() {
        let work_parent = liberado_bootstrap::data_dir().join("goal-workspaces");
        let _ = std::fs::create_dir_all(&work_parent);
        goals_hub.register_pack(Arc::new(liberado_coder_agent::CodingSessionPack::new(
            p.clone(),
            work_parent,
        )));
        info!("goal session packs: life + coding");
    } else {
        info!("goal session packs: life only (no provider — coding pack skipped)");
    }
    let goals = Arc::new(goals_hub);

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
        conversations_root: sessions_root.clone(),
        main_agent_capabilities,
        dispatcher_capabilities,
        config: Arc::new(config.clone()),
        model_name,
        provider: provider.clone(),
        hooks: resolved_hooks,
        hook_tx,
        hook_idempotency: Default::default(),
    });

    // Optional — a proposal Telegram approval bot, answering the Approve/Reject buttons
    // `TelegramNotifier::notify_proposal` sends. Only meaningful when a provider is attached (no
    // provider ⇒ no dispatcher/orchestrator ⇒ no proposals are ever created in the first place),
    // and only runs when TELEGRAM_BOT_TOKEN/TELEGRAM_CHAT_ID are set (same env vars the notifier
    // itself uses). Cloning the vault/signer here, before `daemon` moves into its own spawn below,
    // gives the bot its own handle onto the exact same underlying vault (`Vault` is cheap to
    // clone — see `liberado_vault::Vault`'s doc comment).
    if let Some(p) = provider.as_ref()
        && let Some(bot) = liberado_telegram_approvals::ApprovalBot::from_env(
            daemon.vault().clone(),
            daemon.signer().clone(),
            p.clone(),
            config.tuning.telegram_approvals.clone(),
        )
    {
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
async fn build_chat(
    provider: Option<Arc<dyn Provider>>,
    mcp: Option<McpRegistry>,
    config: &liberado_bootstrap::Config,
    catalog: Arc<CapabilityCatalog>,
    vault_path: &Path,
    guidance: Option<Arc<dyn liberado_common::ToolGuidanceSource>>,
    // The **concrete** converged store, not an erased `dyn ConversationStore`: chat needs both of
    // its lenses — the chat view for its own transcript, and the kernel view to record a `delegate`d
    // subagent as a background session. One trait object cannot be cast to the other.
    store: Arc<SessionStore>,
) -> (Option<Arc<ChatSessions>>, usize, Vec<String>) {
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

    // Orchestrator uses **dispatcher** caps so specialist MCPs are reachable via `delegate`
    // without granting them to the face agent.
    let (runtime, orchestrator) =
        connect_chat_runtime(&provider, mcp, &dispatcher_caps, &guard).await;

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
        store.clone(),
        Executor::new(provider.clone(), Budget::default()),
        runtime,
    )
    .with_system_prompt(system_prompt)
    // A `delegate`d subagent becomes a background session — a child of the chat that asked for it
    // (S5′ step 5). The same store, seen through the kernel's lens.
    .with_session_store(store)
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

    // Dispatch routing: required for face-agent `delegate` and for legacy pre-turn path.
    if let Some(orchestrator) = orchestrator {
        let mut dispatcher = Dispatcher::new(
            provider,
            config.tuning.dispatch.clone(),
            config.tuning.concurrency.max_reaction_depth,
        );
        if let Some(g) = guidance {
            dispatcher = dispatcher.with_guidance(g);
        }
        if main_agent_cfg.delegation_mode {
            info!(
                "chat: face-agent mode — human interfacer + delegate tool (dispatcher routes work)"
            );
        } else {
            info!(
                "chat: legacy dispatch mode (pre-turn routing + main-agent MCP tools on stream path)"
            );
        }
        sessions = sessions.with_dispatch(dispatcher, catalog, orchestrator);
    } else if main_agent_cfg.delegation_mode {
        warn!(
            "chat: delegation_mode is on but no MCP/orchestrator — face agent will have no delegate tool"
        );
    }

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

/// Connect chat's tool runtime once, reused for its lifetime, and — when an MCP registry is
/// configured — build the Orchestrator that backs its dispatch-routed executions
/// (Clarify/Propose/DispatchSubagent; see `build_chat`'s `with_dispatch` wiring). Without an MCP,
/// chat still works as plain conversation and dispatch routing is skipped entirely (there would be
/// nothing for the orchestrator to execute against).
async fn connect_chat_runtime(
    provider: &Arc<dyn Provider>,
    mcp: Option<McpRegistry>,
    capabilities: &CapabilitySet,
    guard: &liberado_bootstrap::GuardContext,
) -> (Arc<dyn ToolRuntime>, Option<Orchestrator>) {
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
            let orchestrator = Orchestrator::new(
                provider.clone(),
                registry,
                capabilities.clone(),
                guard.consequences.clone(),
                guard.zone_catalog.clone(),
                guard.zone_write_classes.clone(),
                guard.proposals_dir.clone(),
                guard.signer.clone(),
                DEFAULT_POOL,
            );
            (rt, Some(orchestrator))
        }
        None => {
            info!("chat: no MCP configured — chat will be conversation-only");
            (Arc::new(NoTools), None)
        }
    }
}
