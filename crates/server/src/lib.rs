//! # liberado-server
//!
//! The Liberado daemon process, as a library: it assembles the provider/chat/daemon (via
//! `liberado_bootstrap`), spawns the vault watch loop, and serves the HTTP/SSE API (`docs/spec/reference/api.md`)
//! plus the built web frontend. The `liberado serve` subcommand calls [`run`]; this crate ships no
//! binary and does not init the global tracing subscriber (the CLI owns that, so a library embedding
//! us doesn't fight over it).

mod api;
mod cron_delivery;
mod main_agent_budget;
use main_agent_budget::main_agent_budget;
mod hooks;
mod latency;
mod shutdown;
mod state;
mod sticky;
mod telegram;

#[cfg(test)]
mod t1_conformance;

use std::sync::Arc;
use std::time::{Duration, Instant};

use std::path::Path;

use axum::Router;
use liberado_common::{CapabilityCatalog, CapabilitySet, WriteProvenance};
use liberado_daemon::Daemon;
use liberado_dispatcher::Dispatcher;
use liberado_executor::{Executor, ToolRuntime};
use liberado_main_agent::ChatSessions;
use liberado_mcp::McpRegistry;
use liberado_provider::Provider;
use liberado_session_store::SessionStore;
use tokio::sync::Mutex;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::{info, warn};

/// Where `dx build --release --package liberado-webui --web` places the built frontend, relative to
/// the repo root. Correct when you run the daemon from a dev checkout; useless in the deploy image,
/// which has no repo and whose working directory is `/` — hence [`dist_dir`].
const DIST_DIR: &str = "target/dx/liberado-webui/release/web/public";

/// Directory the frontend is served from: `LIBERADO_WEBUI_DIST` if set, else [`DIST_DIR`].
///
/// GitHub Actions bakes a bundle into `/usr/share/liberado/webui` and the image sets this env to
/// that path. Homelab Compose may still override it to a `/webui` mount so `just deploy-webui-homelab`
/// can update the UI without rebuilding the daemon image. `setup.sh` points back at the baked path
/// when the pulled image contains `index.html`.
fn dist_dir() -> String {
    std::env::var("LIBERADO_WEBUI_DIST").unwrap_or_else(|_| DIST_DIR.to_string())
}

use crate::state::AppState;

const DEFAULT_PORT: u16 = 4201;

/// Run the daemon over `vault_path`: build the shared provider/MCP, assemble the chat agent and the
/// vault daemon (watch loop), then serve the HTTP/SSE API and static frontend until killed. This is
/// the daemon's foreground entry point — `liberado serve` calls it. The caller is expected to have
/// already initialised the tracing subscriber.
pub async fn run(vault_path: String) -> Result<(), Box<dyn std::error::Error>> {
    let port = resolve_port();

    // Load + validate the config up front (Decision 14 fail-fast). A bad config is a hard error
    // here rather than a half-booted daemon; the message names the file/setting to fix.
    let (config, _) = liberado_bootstrap::load_config(liberado_bootstrap::config_dir().as_deref())?;

    // Resolve the vault path CLI-over-config: the `run` argument wins (the CLI always supplies one);
    // an empty argument falls back to `topology.vault_path`. Both empty is a hard error.
    let vault_path = resolve_vault_path(vault_path, &config)?;

    // One live catalog + shared MCP registry (boot apply + hot-reload). Every consumer (API,
    // daemon pools, chat, dispatch pack) uses the same cloneable registry handle.
    let live_mcp = liberado_bootstrap::live_mcp_from_config(&config, None);
    let capability_catalog = live_mcp.catalog();
    let mcp_registry = live_mcp.registry();

    // Latency journal (records every inference call, off the hot path) + the per-role providers,
    // each role-tagged and metered, built from config. `provider` here is the plain default
    // (`primary`) used for status/model display, the coding pack, and the runtime model-swap API;
    // the daemon/chat get the role-specific providers via `providers`.
    let latency_recorder: Arc<dyn liberado_provider::LatencyRecorder> =
        crate::latency::JsonlLatencyRecorder::spawn();
    let providers = liberado_bootstrap::role_providers_from_config(&config, latency_recorder);
    let provider = providers.primary.clone();
    let dispatcher_attached = provider.is_some();
    let model_name = provider.as_ref().map(|p| p.model().to_string());
    // Orchestrator is always wired with the live registry (possibly empty); emptiness no longer
    // freezes EmptyRuntimeFactory at boot.
    let orchestrator_attached = dispatcher_attached;

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
    let (sessions_root, sessions) = open_session_store().await;

    let goals = build_goal_hub(
        &config,
        &providers,
        provider.as_ref(),
        &vault_path,
        &capability_catalog,
        &mcp_registry,
        &guidance,
        &sessions,
    )
    .await;

    let (chat, chat_tools, chat_tool_names) = build_chat(
        &providers,
        mcp_registry.clone(),
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
        &providers,
        &config,
        capability_catalog.clone(),
        mcp_registry.clone(),
        Path::new(&vault_path),
        guidance,
    )
    // Every reaction is a hosted background session on the hub (E3) — joinable, cancellable.
    .with_goal_hub(goals.clone());

    // Shared state for chat-aware cron delivery (`docs/future-work/ideas/cron-delivery-timing-idea.md`): the
    // sticky Telegram session id (also owned by the chat bridge) and the "last human message" clock
    // (also stamped by the approval bot). Built here so the daemon's delivery notifier and the
    // bot/bridge below all point at the same instances.
    //
    // The sticky id is now **persisted** across restarts (`<data_dir>/telegram-sticky-session`, on the
    // same volume as the session store): on boot we restore the last conversation so a container
    // restart no longer forces an implicit `/new`. A restored id is adopted only if the conversation
    // still exists (validated against the chat store) — a stale pointer is dropped, not resurrected.
    let (telegram_sticky, telegram_activity) = resolve_telegram_state(chat.as_ref()).await;
    let daemon = wrap_cron_notifier(
        daemon,
        chat.as_ref(),
        &config,
        &telegram_sticky,
        &telegram_activity,
    );

    // The webhook hooks endpoint's seam into the daemon's reactive pipeline — a clone of the same
    // channel every `EventSource` (vault-watch, cron) pushes onto. Grabbed before `daemon` moves
    // into its own spawned task below, same pattern as the vault/signer clones just above it.
    let hook_tx = daemon.event_sender();
    // Same constraint as `event_sender` above: grab it before `daemon` moves into its task.
    let watcher_active = daemon.watcher_health();
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
        watcher_active,
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
        data_dir: liberado_config::data_dir(),
        main_agent_capabilities,
        dispatcher_capabilities,
        config: Arc::new(config.clone()),
        model_name,
        provider: provider.clone(),
        hooks: resolved_hooks,
        hook_tx,
        hook_idempotency: Default::default(),
        live_mcp: live_mcp.clone(),
        drain: crate::shutdown::DrainGate::default(),
    });

    spawn_telegram_bot(
        &state,
        &daemon,
        provider.as_ref(),
        &config,
        &telegram_sticky,
        telegram_activity,
    );

    let reaction_tx = state.reaction_tx();
    let daemon_handle = tokio::spawn(async move {
        daemon.run(reaction_tx).await.ok();
    });

    let app = build_app_router(&state);

    let addr = format!("0.0.0.0:{port}");
    serve_with_drain(app, &state, &addr).await?;

    // Vault-watch / reaction loop: stop after chat drain so cron/vault reactions do not keep the
    // process alive past the grace budget.
    daemon_handle.abort();
    Ok(())
}

/// The converged Session store (S5′ / D7): ONE store under `<data_dir>/sessions/`, handed to
/// *both* chat and the goal-session hub. A chat and a goal session are the same record with a
/// different `goal: Option` — so they share an id space, a directory, and a log format. Chat sees
/// it through `ConversationStore`, the kernel through `SessionRecordStore`; neither knows the
/// other is there.
///
/// The previous `<data_dir>/conversations/` and `<data_dir>/goal-sessions/` directories are left
/// untouched but no longer read (deliberate: fresh start, nothing destroyed).
///
/// One `sessions_dir()`, not a `.join("sessions")` here and another in the `chat-search` MCP —
/// that is precisely how the MCP got left behind pointing at the dead `conversations/` directory.
///
/// Also spawns the incognito backstop: the WebUI deletes its own ephemeral chats on the way out,
/// and this sweep is what makes "almost" not the end of the story for the ones it never got to
/// discard. Nothing here touches the disk: an ephemeral session has no file to remove.
async fn open_session_store() -> (std::path::PathBuf, Arc<SessionStore>) {
    let sessions_root = liberado_bootstrap::sessions_dir();
    let sessions = Arc::new(liberado_session_store::SessionStore::open(&sessions_root).await);

    // Backstop for incognito chats whose surface never got to discard them — a closed laptop, a
    // killed tab, a dropped connection. The WebUI deletes its own on the way out and that is what
    // runs almost every time; this is what makes "almost" not the end of the story, because an
    // incognito transcript sitting in daemon RAM until the next restart is exactly the thing the
    // mode promises not to do.
    {
        const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(5 * 60);
        // Generous next to the sweep interval: this is the abandonment threshold, not an idle
        // timeout, and a chat you walked away from mid-thought should still be there when you come
        // back from lunch-adjacent distances.
        const IDLE_BEFORE_SWEEP: std::time::Duration = std::time::Duration::from_secs(30 * 60);
        let sessions = Arc::clone(&sessions);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(SWEEP_EVERY);
            loop {
                ticker.tick().await;
                sessions.sweep_ephemeral(IDLE_BEFORE_SWEEP).await;
            }
        });
    }
    (sessions_root, sessions)
}

/// Shared state for chat-aware cron delivery
/// (`docs/future-work/ideas/cron-delivery-timing-idea.md`): the sticky Telegram session id (also
/// owned by the chat bridge) and the "last human message" clock (also stamped by the approval
/// bot). Built here so the daemon's delivery notifier and the bot/bridge below all point at the
/// same instances.
///
/// The sticky id is now **persisted** across restarts (`<data_dir>/telegram-sticky-session`, on
/// the same volume as the session store): on boot we restore the last conversation so a container
/// restart no longer forces an implicit `/new`. A restored id is adopted only if the conversation
/// still exists (validated against the chat store) — a stale pointer is dropped, not resurrected.
async fn resolve_telegram_state(
    chat: Option<&Arc<ChatSessions>>,
) -> (sticky::StickySession, Arc<Mutex<Option<Instant>>>) {
    let telegram_sticky = if let Some(chat_sessions) = chat {
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
    (telegram_sticky, telegram_activity)
}

/// When both Telegram and a chat surface exist, route the daemon's cron delivery through the
/// chat-delivering notifier: it folds each brief into the sticky conversation and defers the send
/// around active chat. Proposals still go out immediately (its inner notifier). Without chat or
/// Telegram, the daemon keeps whatever `configure_daemon` set (plain immediate notify).
fn wrap_cron_notifier(
    daemon: Daemon,
    chat: Option<&Arc<ChatSessions>>,
    config: &liberado_bootstrap::Config,
    telegram_sticky: &sticky::StickySession,
    telegram_activity: &Arc<Mutex<Option<Instant>>>,
) -> Daemon {
    match (chat, liberado_notify::TelegramNotifier::from_env()) {
        (Some(chat_sessions), Some(inner)) => {
            let cdn = cron_delivery::ChatDeliveringNotifier::new(
                Arc::new(inner),
                chat_sessions.clone(),
                telegram_sticky.clone(),
                telegram_activity.clone(),
                Duration::from_secs(config.tuning.cron_delivery.quiet_delay_secs),
                Duration::from_secs(config.tuning.cron_delivery.deliver_by_secs),
            );
            info!(
                "cron delivery: folding briefs into the sticky Telegram chat (quiet-delay defer)"
            );
            daemon.with_notifier(Arc::new(cdn))
        }
        _ => daemon,
    }
}

/// Optional — Telegram bot: proposal Approve/Reject/Revise buttons + free-form chat when a
/// ChatSessions surface exists. Only when a provider is attached and LIBERADO_TELEGRAM_* env vars
/// are set. Cloning the vault/signer here, before `daemon` moves into its own spawn, gives the
/// bot its own handle onto the same vault (`Vault` is cheap to clone).
fn spawn_telegram_bot(
    state: &Arc<AppState>,
    daemon: &Daemon,
    provider: Option<&Arc<dyn Provider>>,
    config: &liberado_bootstrap::Config,
    telegram_sticky: &sticky::StickySession,
    telegram_activity: Arc<Mutex<Option<Instant>>>,
) {
    let Some(p) = provider else {
        return;
    };
    let Some(mut bot) = liberado_telegram_approvals::ApprovalBot::from_env(
        daemon.vault().clone(),
        daemon.signer().clone(),
        p.clone(),
        config.tuning.telegram_approvals.clone(),
    )
    // The same ledger the daemon reads. A tap is the authenticated act; the vault note it also
    // updates is only the human-readable view of a decision recorded here.
    .map(|b| {
        b.with_approval_ledger(liberado_common::ApprovalLedger::new(
            liberado_config::data_dir(),
        ))
    }) else {
        return;
    };
    if state.chat.is_some() {
        // The shared slash-command catalog (also used by the TUI/WebUI), curated to the
        // top-level commands Telegram can advertise, so typing `/` shows an autocomplete menu.
        let command_menu = liberado_commands::telegram_commands()
            .into_iter()
            .map(|(c, d)| (c.to_string(), d.to_string()))
            .collect();
        bot = bot
            .with_chat(Arc::new(crate::telegram::TelegramChatBridge {
                state: state.clone(),
                session_id: telegram_sticky.clone(),
            }))
            .with_activity_tracker(telegram_activity)
            .with_command_menu(command_menu);
        info!(
            "Telegram free-form chat surface attached (slash commands enabled + menu registered)"
        );
    }
    tokio::spawn(bot.run());
}

/// The full HTTP/SSE API router plus the static frontend fallback. Work-starting routes are
/// grouped behind the drain middleware (it refuses with `shutting_down` once drain begins);
/// attach/cancel/park/list stay on the main router so clients can rejoin or stop work already in
/// flight. `POST /api/goals` is gated for the same reason as chat — the gate is the capability,
/// not the surface that happened to be wired first.
fn build_app_router(state: &Arc<AppState>) -> Router {
    let work_start_routes = Router::new()
        .route("/api/chat", axum::routing::post(api::chat))
        .route(
            "/api/chat/stream",
            axum::routing::get(api::chat_stream_get).post(api::chat_stream_post),
        )
        .route("/api/goals", axum::routing::post(api::goals_start))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            shutdown::refuse_new_turns_if_draining,
        ))
        .with_state(state.clone());

    Router::new()
        .merge(work_start_routes)
        .route("/api/status", axum::routing::get(api::status))
        .route("/api/models", axum::routing::get(api::models))
        .route("/api/models/select", axum::routing::post(api::select_model))
        .route("/api/catalog", axum::routing::get(api::catalog))
        .route(
            "/api/mcp/reload",
            axum::routing::post(api::reload_mcp_peers),
        )
        .route("/api/reactions", axum::routing::get(api::reactions))
        .route("/api/vault", axum::routing::get(api::vault))
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
            axum::routing::get(api::get_conversation)
                .patch(api::patch_conversation_title)
                .delete(api::delete_conversation),
        )
        // Surface-only authority change. Deliberately POST, and deliberately absent from every tool
        // catalog — see the handler's docs for why both halves matter.
        .route(
            "/api/conversations/{id}/profile",
            axum::routing::post(api::set_conversation_profile),
        )
        // Rejoin a turn after a reload, and stop one on purpose. Both exist because a turn no
        // longer belongs to the connection that started it. Not gated by drain.
        .route(
            "/api/conversations/{id}/attach",
            axum::routing::get(api::attach_conversation),
        )
        .route(
            "/api/conversations/{id}/cancel",
            axum::routing::post(api::cancel_conversation_turn),
        )
        .route("/api/profiles", axum::routing::get(api::list_profiles))
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
        // Coding project picker (S3/G4) — read-only; start still drain-gated.
        .route("/api/projects", axum::routing::get(api::list_projects))
        // List only — start is on `work_start_routes` (drain-gated).
        .route("/api/goals", axum::routing::get(api::goals_list))
        .route("/api/goals/{id}", axum::routing::get(api::goals_get))
        .route("/api/goals/{id}/diff", axum::routing::get(api::goals_diff))
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
        .route("/api/goals/{id}/park", axum::routing::post(api::goals_park))
        .route(
            "/api/goals/{id}/rewind",
            axum::routing::post(api::goals_rewind),
        )
        .layer(CorsLayer::permissive())
        .with_state(state.clone())
        // Compression is scoped to the static fallback, deliberately not applied to the router as
        // a whole. The payload that needs it is the release .wasm (multi-MB, ~4x compressible, and
        // the whole page blocks on it over the tailnet); the payload that must never be buffered
        // is `/api/chat/stream`, where holding bytes back turns a live turn into a frozen UI.
        // Scoping it here makes that impossible by construction rather than by trusting a
        // predicate.
        .fallback_service(
            tower::ServiceBuilder::new()
                .layer(CompressionLayer::new())
                .service(ServeDir::new(dist_dir())),
        )
}

/// Bind and serve the router with a graceful shutdown: on SIGTERM/Ctrl+C, drain refuses new turns
/// for up to the grace period, then the HTTP accept stops.
async fn serve_with_drain(
    app: Router,
    state: &Arc<AppState>,
    addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let grace = shutdown::shutdown_grace_from_env();
    info!("Web UI server listening on http://{}", addr);
    info!(
        grace_secs = grace.as_secs(),
        "shutdown: on SIGTERM/Ctrl+C drain refuses new turns for up to grace_secs then exits \
         (set LIBERADO_SHUTDOWN_GRACE_SECS; compose stop_grace_period should be ≥ this)"
    );
    log_endpoint_summary();

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let drain_state = state.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown::wait_for_shutdown_signal().await;
            let outcome = shutdown::drain_for_shutdown(&drain_state, grace).await;
            info!(
                idle = outcome.idle_within_grace,
                aborted = outcome.aborted,
                parked_goals = outcome.parked_goals,
                waited_ms = outcome.waited.as_millis() as u64,
                "shutdown drain complete; stopping HTTP accept"
            );
        })
        .await?;
    Ok(())
}

/// The port the daemon serves on: `LIBERADO_PORT` if set and parseable, else [`DEFAULT_PORT`].
fn resolve_port() -> u16 {
    std::env::var("LIBERADO_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// The vault path this daemon serves, CLI-over-config: the `run` argument wins (the CLI always
/// supplies one); an empty argument falls back to `topology.vault_path`. Both empty is a hard
/// error.
fn resolve_vault_path(
    vault_path: String,
    config: &liberado_bootstrap::Config,
) -> Result<String, Box<dyn std::error::Error>> {
    if !vault_path.trim().is_empty() {
        return Ok(vault_path);
    }
    let from_config = config.topology.vault_path.to_string_lossy().into_owned();
    if from_config.trim().is_empty() {
        return Err(
            "no vault path: pass one to `liberado serve` or set topology.vault_path".into(),
        );
    }
    Ok(from_config)
}

/// The goal-session hub — the **one** execution engine (one-execution-engine plan E3/E4). Life-ops
/// demo always; coding when a provider is available; dispatch pack so cron/webhook/delegate are
/// hosted sessions, not a second engine. Built before chat so `delegate` can use it. Also attaches
/// the out-of-band alert (E5) and reconciles orphaned parked sessions at startup (F7).
#[allow(clippy::too_many_arguments)]
async fn build_goal_hub(
    config: &liberado_bootstrap::Config,
    providers: &liberado_bootstrap::RoleProviders,
    provider: Option<&Arc<dyn Provider>>,
    vault_path: &str,
    capability_catalog: &Arc<CapabilityCatalog>,
    mcp_registry: &McpRegistry,
    guidance: &Option<Arc<dyn liberado_common::ToolGuidanceSource>>,
    sessions: &Arc<SessionStore>,
) -> Arc<liberado_session::GoalSessionHub> {
    let mut goals_hub = liberado_session::GoalSessionHub::new(SessionStore::clone(sessions));
    let coding_pack = register_goal_packs(
        &mut goals_hub,
        config,
        providers,
        provider,
        vault_path,
        capability_catalog,
        mcp_registry,
        guidance,
    );
    finalize_goal_hub(goals_hub, config, coding_pack).await
}

/// Register the goal-hub's domain packs: life-ops demo always; coding when a provider is
/// attached; dispatch pack so cron/webhook/delegate are hosted sessions, not a second engine.
/// Returns the coding pack (held so the hub can be attached after `Arc::new`, S6 child goal
/// sessions).
#[allow(clippy::too_many_arguments)]
fn register_goal_packs(
    goals_hub: &mut liberado_session::GoalSessionHub,
    config: &liberado_bootstrap::Config,
    providers: &liberado_bootstrap::RoleProviders,
    provider: Option<&Arc<dyn Provider>>,
    vault_path: &str,
    capability_catalog: &Arc<CapabilityCatalog>,
    mcp_registry: &McpRegistry,
    guidance: &Option<Arc<dyn liberado_common::ToolGuidanceSource>>,
) -> Option<Arc<liberado_coder_agent::CodingSessionPack>> {
    goals_hub.register_pack(Arc::new(liberado_session::LifeOpsDemoRunner));
    // Coding pack: hold Arc so we can attach the hub after `Arc::new` (S6 child goal sessions).
    let coding_pack = build_coding_pack(provider, config);
    if let Some(pack) = coding_pack.as_ref() {
        goals_hub.register_pack(Arc::clone(pack) as Arc<dyn liberado_session::DomainPackRunner>);
    }
    if let Some(pack) = liberado_bootstrap::build_dispatch_pack(
        providers,
        config,
        Arc::clone(capability_catalog),
        mcp_registry.clone(),
        Path::new(vault_path),
        guidance.clone(),
    ) {
        goals_hub.register_pack(Arc::new(pack));
        info!("goal session packs: life + coding + dispatch");
    } else if provider.is_some() {
        info!("goal session packs: life + coding (no dispatch pack)");
    } else {
        info!("goal session packs: life only (no provider)");
    }
    coding_pack
}

/// Tail of goal-hub assembly: attach the out-of-band alert (E5), reconcile orphaned parked
/// sessions at startup (F7), and attach the hub to the coding pack for subagent fan-out (S6).
async fn finalize_goal_hub(
    mut goals_hub: liberado_session::GoalSessionHub,
    config: &liberado_bootstrap::Config,
    coding_pack: Option<Arc<liberado_coder_agent::CodingSessionPack>>,
) -> Arc<liberado_session::GoalSessionHub> {
    // E5: when a session awaits input and nobody has the stream open, ping out-of-band.
    if let Some(n) = liberado_notify::TelegramNotifier::from_env() {
        goals_hub = goals_hub.with_alert(Arc::new(NotifySessionAlert(Arc::new(n))));
        info!("session alerts: telegram notifier attached");
    }
    // F7: parked rows survive restart; the hub does not. Finish orphans that cannot be resumed
    // (no AskHuman, no pack, or pack refuses) so they do not sit forever. Human-resumable parks
    // stay for the stuck panel / answer path.
    let reconciled = goals_hub.reconcile_parked_at_startup().await;
    if reconciled > 0 {
        info!(
            reconciled,
            "startup: cancelled orphaned parked sessions with no resume path"
        );
    }
    let goals = Arc::new(goals_hub);
    attach_coding_pack_hub(&goals, &coding_pack, config);
    goals
}

/// S6: coding fan-out spawns child goal sessions on this same hub — attach it once the hub is
/// fully assembled.
fn attach_coding_pack_hub(
    goals: &Arc<liberado_session::GoalSessionHub>,
    coding_pack: &Option<Arc<liberado_coder_agent::CodingSessionPack>>,
    config: &liberado_bootstrap::Config,
) {
    if let Some(pack) = coding_pack.as_ref() {
        pack.attach_hub(Arc::clone(goals));
        info!(
            max_concurrent = config.tuning.dispatch.max_concurrent_coding_subagents,
            "coding pack: hub attached for subagent fan-out"
        );
    }
}

/// Print the API endpoint summary once at boot. One record with embedded newlines: tracing's
/// `info!` expansion is expensive for clippy's cognitive-complexity model (roughly 8 per call),
/// and each line here is a listing, not a decision.
fn log_endpoint_summary() {
    info!(
        "API endpoints:\n  GET /api/status  — daemon status\n  GET /api/models  — live provider \
         model catalog\n  POST /api/models/select  — hot-swap active model\n  \
         GET /api/reactions?limit=20  — recent reactions\n  GET /api/vault  — vault info\n  \
         GET /api/goals  — list goal sessions; POST /api/goals starts one (drain-gated)\n  \
         GET /api/goals/{{id}}/stream  — SSE goal session events\n  /  — static frontend (build \
         with `dx build` from crates/webui/)"
    );
}

/// Build the coding pack for the goal hub, when a provider is attached. A malformed `[coder]`
/// section used to be swallowed by `if let Ok(..)`, so the pack silently kept its defaults and the
/// operator's settings did nothing — with no line anywhere saying why. Cost an hour of "why is my
/// configured model being ignored"; say it out loud instead.
fn build_coding_pack(
    provider: Option<&Arc<dyn Provider>>,
    config: &liberado_bootstrap::Config,
) -> Option<Arc<liberado_coder_agent::CodingSessionPack>> {
    let p = provider?;
    let work_parent = liberado_bootstrap::data_dir().join("goal-workspaces");
    let _ = std::fs::create_dir_all(&work_parent);
    let mut pack = liberado_coder_agent::CodingSessionPack::new(p.clone(), work_parent)
        .with_max_concurrent_coding_subagents(
            config.tuning.dispatch.max_concurrent_coding_subagents,
        );
    match liberado_coder_core::CoderTuning::from_value(config.tuning.coder.as_ref()) {
        Ok(coder_tuning) => {
            // One call: keeps pack fields and the shared production assembly path
            // on the same CoderTuning (backlog 0.4).
            pack = pack.with_tuning(coder_tuning);
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "[coder] tuning section is invalid and was IGNORED; the coding pack is running on built-in defaults, not your config"
            );
        }
    }
    // Without this the pack's provider factory returns the daemon's provider for every
    // role, so the coder's configured model is ignored and the run reports a model
    // name nothing resolves.
    if let Some(factory) = liberado_bootstrap::CoderRoleProviderFactory::for_config(config) {
        pack = pack.with_provider_factory(std::sync::Arc::new(factory));
    }
    Some(Arc::new(pack))
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
            // Printed unconditionally, including the "(none)" case. A silently-absent sink is the
            // shape of a real incident: the code shipped, `topology.toml` is a host mount the
            // deploy script does not touch, and a `Delivery::Vault` report quietly downgraded to a
            // chat summary with nothing but a debug log to say why. A deploy smoke check greps
            // this line, so "did my config actually reach the box" is one command.
            match &config.topology.report_sink {
                Some(sink) => println!(
                    "  report sink: {}:{}                  [{top_src}]",
                    sink.mcp, sink.tool
                ),
                None => println!("  report sink: (none — vault delivery unavailable)"),
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("config check failed: {e}");
            Err(Box::new(e))
        }
    }
}

mod explain_write;
pub use explain_write::explain_write;
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
    providers: &liberado_bootstrap::RoleProviders,
    mcp: McpRegistry,
    config: &liberado_bootstrap::Config,
    catalog: Arc<CapabilityCatalog>,
    vault_path: &Path,
    guidance: Option<Arc<dyn liberado_common::ToolGuidanceSource>>,
    engine: SessionEngine,
) -> (Option<Arc<ChatSessions>>, usize, Vec<String>) {
    let SessionEngine { store, goals } = engine;
    // The chat face runs on the `main_agent` role provider; its `delegate` classifier uses the
    // `dispatcher` role provider (falling back to the face provider if somehow unset).
    let Some(face_provider) = providers.face.clone() else {
        return (None, 0, Vec::new());
    };
    let dispatcher_provider = providers
        .dispatcher
        .clone()
        .unwrap_or_else(|| face_provider.clone());
    let provider = face_provider;

    // Main-agent = chat face surface (usually thin). Dispatcher = worker ceiling for delegated tools.
    let main_agent_caps = config.policy.capabilities_for("main-agent");
    let dispatcher_caps = config.policy.capabilities_for("dispatcher");
    let main_agent_cfg = &config.topology.main_agent;

    // Consequence catalog + proposals dir: shared with `configure_daemon`'s own Orchestrator wiring.
    // Snapshots seed guards; `with_live_catalog` keeps consequence/zone lookups hot-reload safe.
    let guard = liberado_bootstrap::guard_context(&catalog, &config.policy, vault_path);
    let catalog_is_empty = catalog.is_empty();

    // Face-agent tool surface: live registry (refresh on peer-set change). Specialist work goes
    // through `delegate` → hub → dispatch pack, not the face agent's own tool list.
    let runtime = connect_chat_runtime(mcp);

    let (tool_names, tool_count) =
        face_tool_surface(&runtime, main_agent_cfg.delegation_mode, &main_agent_caps);

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
        Executor::new(provider.clone(), main_agent_budget(main_agent_cfg)),
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
    .with_risk_waivers(config.policy.risk_waiver_set())
    .with_live_catalog(catalog.clone())
    .with_dispatcher_capabilities(dispatcher_caps)
    .with_delegation_mode(main_agent_cfg.delegation_mode);

    // CH3 context compaction: config-tier knobs → kernel runtime type (per-model absolute
    // triggers + daemon default). See `state::compaction_config_for_face`. Summaries use the face
    // provider (see crates/main-agent/src/compaction.rs).
    let face_model = provider.model();
    let compact_cfg = crate::state::compaction_config_for_face(config, face_model.as_str());
    let compact_enabled = compact_cfg.enabled;
    let default_trigger = compact_cfg.trigger_tokens;
    let models_with_triggers = compact_cfg.model_trigger_tokens.len();
    let trigger_pct = main_agent_cfg.compaction.trigger_pct;
    let keep_recent_turns = compact_cfg.keep_recent_turns;
    sessions = sessions.with_compaction(compact_cfg, provider.clone());
    if compact_enabled {
        info!(
            face_model = %face_model,
            trigger_tokens = default_trigger,
            models_with_triggers,
            trigger_pct,
            keep_recent_turns,
            "chat: automatic context compaction enabled (per-conversation model triggers)"
        );
    }

    if !catalog_is_empty {
        info!(
            count = consequence_count,
            "chat: runtime safety guards enabled (capability-scoped tools + RiskGatedToolRuntime)"
        );
    }

    // Pre-turn classification (legacy mode) + face-agent `delegate` needs a dispatcher for the
    // classifier; execution is always the hub's dispatch pack.
    let dispatcher = chat_dispatcher(
        dispatcher_provider,
        config.tuning.dispatch.clone(),
        config.tuning.concurrency.max_reaction_depth,
        guidance,
        main_agent_cfg.delegation_mode,
    );
    sessions = sessions.with_dispatch(dispatcher, catalog);

    (Some(Arc::new(sessions)), tool_count, tool_names)
}

/// The face agent's tool surface: `delegate` only (plus granted main-agent MCP tools) in
/// delegation mode, the full live registry otherwise.
fn face_tool_surface(
    runtime: &Arc<dyn ToolRuntime>,
    delegation_mode: bool,
    caps: &CapabilitySet,
) -> (Vec<String>, usize) {
    let mut tool_names: Vec<String> = runtime.catalog().iter().map(|t| t.name.clone()).collect();
    if delegation_mode {
        tool_names = vec![liberado_main_agent::DELEGATE_TOOL_NAME.to_string()];
        let granted = caps.granted_mcps();
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
            delegation_mode,
            "chat: tool surface ready"
        );
    } else {
        info!("chat: no tools available — the model can only converse, not act");
    }
    (tool_names, tool_count)
}

/// A dispatcher wired with the given guidance and delegation mode (which only changes the
/// startup log line — the mode is applied downstream by `with_delegation_mode`).
fn chat_dispatcher(
    dispatcher_provider: Arc<dyn Provider>,
    dispatch_tuning: liberado_config::DispatchTuning,
    max_reaction_depth: u32,
    guidance: Option<Arc<dyn liberado_common::ToolGuidanceSource>>,
    delegation_mode: bool,
) -> Dispatcher {
    let mut dispatcher = Dispatcher::new(dispatcher_provider, dispatch_tuning, max_reaction_depth);
    if let Some(g) = guidance {
        dispatcher = dispatcher.with_guidance(g);
    }
    if delegation_mode {
        info!("chat: face-agent mode — human interfacer + delegate tool (hub hosts work)");
    } else {
        info!(
            "chat: legacy dispatch mode (pre-turn routing + main-agent MCP tools on stream path)"
        );
    }
    dispatcher
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

    let vault = open_guidance_vault(vault_path).await?;
    let embedder = load_guidance_embedder()?;

    match open_procedural_memory(vault, embedder).await {
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

/// Open the vault backing the dispatcher's procedural memory. Any failure (bad vault path, model
/// load error) degrades to `None` — this is an optimization, never something worth failing boot
/// over.
async fn open_guidance_vault(vault_path: &str) -> Option<liberado_vault::Vault> {
    match liberado_vault::Vault::open("dispatcher-guidance", vault_path).await {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(error = %e, "dispatcher guidance: failed to open vault — continuing without it");
            None
        }
    }
}

/// Load the embedding model for procedural-memory retrieval (`LIBERADO_MEMORY_MODEL`, defaulting
/// to bge-small-en-v1.5). A failed load degrades to `None` — the same store
/// `liberado-memory-mcp` (a separate subprocess) already exposes to agents, so an unopted-in
/// deployment isn't paying for a second copy of that model just to run `liberado serve`.
fn load_guidance_embedder() -> Option<Arc<dyn turbovault_vector::EmbeddingEngine>> {
    let model =
        std::env::var("LIBERADO_MEMORY_MODEL").unwrap_or_else(|_| "bge-small-en-v1.5".to_string());
    match turbovault_vector::FastembedEngine::new(&model, None) {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            warn!(error = %e, "dispatcher guidance: failed to load embedding model — continuing without it");
            None
        }
    }
}

/// Open the procedural-memory store over an already-open vault and embedder.
async fn open_procedural_memory(
    vault: liberado_vault::Vault,
    embedder: Arc<dyn turbovault_vector::EmbeddingEngine>,
) -> Result<liberado_memory_store::MemoryStore, liberado_memory_store::MemoryError> {
    liberado_memory_store::MemoryStore::open(
        vault,
        "memory/procedural",
        embedder,
        None,
        liberado_memory_store::MemoryStoreConfig::default(),
    )
    .await
}

/// Chat face tools: live registry handle that re-connects when the peer set changes (empty→add
/// after `POST /api/mcp/reload` works without process restart).
fn connect_chat_runtime(mcp: McpRegistry) -> Arc<dyn ToolRuntime> {
    info!(
        peers = mcp.len(),
        "chat: live MCP registry runtime (refreshes on peer-set change)"
    );
    Arc::new(liberado_mcp::LiveRegistryRuntime::new(
        mcp,
        WriteProvenance::agent("liberado-chat", "chat-session"),
    ))
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

/// `liberado prompt [profile]` — print the system prompt a chat under `profile` would actually be
/// given, composed from config alone.
///
/// # Why this exists
///
/// Three bugs in one evening (2026-07-28) were all the same question: *what is the model being told,
/// and does it match what it is being handed?* A `basic-chat` session was handed the face-agent
/// prompt while holding no `delegate`, so it announced work and did none; then it was told it had no
/// tools while its grant plainly listed two. Each took a ~17-minute build and a live run to see.
/// None of them needed a daemon to diagnose — every one was visible in the composed prompt.
///
/// This is the seam-sweep argument applied to prompts: stop deploying to find out what you are
/// sending.
///
/// # What it can and cannot resolve
///
/// Config knows a per-tool grant exactly — `ExecuteTool("turbovault:tasks_list")` *is* the tool name.
/// It cannot expand a whole-server grant, which becomes whatever that server currently advertises
/// and is known only to a connected daemon. Those print as `<mcp>:*` and are marked, rather than
/// quietly rendered as if they were resolved names: a debugging tool that hides its own uncertainty
/// is worse than one that admits it. A `--live` variant against a running daemon can share this
/// renderer later; the config version is the one that runs in CI and mid-debug, before deploying.
///
/// The prompt body comes from [`liberado_main_agent::tool_manifest`] — the same function the turn
/// injects — so this cannot drift from what the model reads.
pub fn show_prompt(
    dir: Option<&Path>,
    profile: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = dir
        .map(Path::to_path_buf)
        .or_else(liberado_bootstrap::config_dir);
    let (config, _) = liberado_bootstrap::load_config(resolved.as_deref())?;
    print!("{}", compose_chat_prompt(&config, profile)?.render());
    Ok(())
}

/// The system-prompt view a chat under a given profile would be given.
///
/// Returned rather than printed so it can be asserted on. `show_prompt` is the thin printing
/// wrapper; every property worth testing lives here. A composer that could only print would be
/// checkable solely by running the binary and reading it — which is the situation this command
/// exists to end, reproduced one level down.
pub struct ChatPromptPreview {
    pub profile: Option<String>,
    pub delegation: bool,
    pub model: Option<String>,
    /// Whole-server grants, whose tool lists only a connected daemon can expand.
    pub unresolved_mcps: Vec<String>,
    /// The system messages the turn would assemble, **in order**: base prompt, the profile's nudge
    /// if it has one, then the tool manifest. Mirrors what `ChatSessions` injects per turn.
    pub system_messages: Vec<String>,
}

impl ChatPromptPreview {
    /// The human-readable report `liberado prompt` prints.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let rule = "─".repeat(76);
        let mut out = String::new();
        let label = self
            .profile
            .as_deref()
            .unwrap_or("(no profile — the daemon default)");
        let _ = writeln!(out, "system prompt for a chat under: {label}");
        let _ = writeln!(out, "  delegation: {}", self.delegation);
        if let Some(model) = &self.model {
            let _ = writeln!(
                out,
                "  model:      {model}  (this profile's turns run on it)"
            );
        }
        for mcp in &self.unresolved_mcps {
            let _ = writeln!(
                out,
                "  note:       '{mcp}' is a whole-server grant; its tools resolve at runtime"
            );
        }
        // Says ceiling, not turn. `chat turn: tool surface` logs what a turn *actually* held after
        // the dispatcher narrows to the goal's relevant MCPs, and the two legitimately differ — a
        // basic-chat profile listing three tools ran a turn holding two (live, 2026-08-01). Without
        // this line, someone comparing the two concludes the inspector is lying.
        let _ = writeln!(
            out,
            "  scope:      the profile's ceiling; a turn may hold fewer after per-goal narrowing"
        );
        let _ = writeln!(out, "\n{rule}");
        let _ = writeln!(out, "{}", self.system_messages.join("\n\n"));
        let _ = writeln!(out, "{rule}");
        out
    }
}

/// Compose what a chat under `profile` would be told, from `config` alone.
///
/// Mirrors the turn's own assembly order deliberately: base prompt, profile nudge, tool manifest
/// last. If that order ever diverges from `ChatSessions`, this command starts lying — which is why
/// the manifest text itself comes from [`liberado_main_agent::tool_manifest`] rather than being
/// re-worded here.
pub fn compose_chat_prompt(
    config: &liberado_config::Config,
    profile: Option<&str>,
) -> Result<ChatPromptPreview, Box<dyn std::error::Error>> {
    use liberado_common::Capability;

    // "main-agent" is the fallback grant a chat with no profile runs under — the same component
    // `run` reads at boot. A wrong fallback here would make the no-profile case a fiction.
    let resolved = config.resolve_session_profile(profile, "main-agent")?;
    let main_agent = &config.topology.main_agent;

    // Delegation decides which built-in prompt applies, exactly as the daemon decides it: the
    // profile's setting when it states one, else the daemon default.
    let delegation = resolved.delegation.unwrap_or(main_agent.delegation_mode);
    let base = main_agent.system_prompt.clone().unwrap_or_else(|| {
        if delegation {
            liberado_main_agent::HUMAN_INTERFACE_SYSTEM_PROMPT.to_string()
        } else {
            liberado_main_agent::DEFAULT_SYSTEM_PROMPT.to_string()
        }
    });

    // Per-tool grants are exact; whole-server grants are not resolvable without a live catalog, so
    // they print as `<mcp>:*` and are called out rather than quietly rendered as resolved names.
    let mut names: Vec<String> = Vec::new();
    let mut unresolved_mcps: Vec<String> = Vec::new();
    for capability in &resolved.capabilities.capabilities {
        match capability {
            Capability::ExecuteTool(qualified) => names.push(qualified.clone()),
            Capability::ExecuteMcp(mcp) => {
                names.push(format!("{mcp}:*"));
                unresolved_mcps.push(mcp.clone());
            }
            _ => {}
        }
    }
    // `delegate` is not a granted MCP — it is built in, and only when the session may delegate.
    if delegation {
        names.insert(0, liberado_main_agent::DELEGATE_TOOL_NAME.to_string());
    }

    let mut system_messages = vec![base];
    let nudge: Option<&str> = resolved.prompt_append.as_deref().map(str::trim);
    if let Some(extra) = nudge.filter(|t| !t.is_empty()) {
        system_messages.push(extra.to_string());
    }
    let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
    system_messages.push(liberado_main_agent::tool_manifest(&borrowed));

    Ok(ChatPromptPreview {
        profile: profile.map(str::to_string),
        delegation,
        model: resolved.model,
        unresolved_mcps,
        system_messages,
    })
}

#[cfg(test)]
#[path = "lib_prompt_preview_tests.rs"]
mod prompt_preview_tests;

#[cfg(test)]
#[path = "lib_startup_reconciliation_tests.rs"]
mod startup_reconciliation_tests;

#[cfg(test)]
#[path = "lib_boot_helper_tests.rs"]
mod boot_helper_tests;
