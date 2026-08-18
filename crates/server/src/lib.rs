//! # liberado-server
//!
//! The Liberado daemon process, as a library: it assembles the provider/chat/daemon (via
//! `liberado_bootstrap`), spawns the vault watch loop, and serves the HTTP/SSE API (`docs/spec/reference/api.md`)
//! plus the built web frontend. The `liberado serve` subcommand calls [`run`]; this crate ships no
//! binary and does not init the global tracing subscriber (the CLI owns that, so a library embedding
//! us doesn't fight over it).

mod api;
mod cron_delivery;
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
use liberado_executor::{Budget, Executor, ToolRuntime};
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
/// The homelab mounts the bundle into the container and points this at the mount, because the UI is
/// built on a dev machine (it needs the wasm32 toolchain, which the deploy image does not carry) and
/// shipped separately from the binary. Keeping it a mount rather than an image layer also means a UI
/// redeploy is a file copy — `ServeDir` reads per request, so no restart is needed.
fn dist_dir() -> String {
    std::env::var("LIBERADO_WEBUI_DIST").unwrap_or_else(|_| DIST_DIR.to_string())
}

use crate::state::AppState;

const DEFAULT_PORT: u16 = 4201;

/// Run the daemon over `vault_path`: build the shared provider/MCP, assemble the chat agent and the
/// vault daemon (watch loop), then serve the HTTP/SSE API and static frontend until killed. This is
/// the daemon's foreground entry point — `liberado serve` calls it. The caller is expected to have
/// already initialised the tracing subscriber.
#[allow(clippy::cognitive_complexity)]
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
    let sessions_root = liberado_bootstrap::sessions_dir();
    let sessions = Arc::new(liberado_session_store::SessionStore::open(&sessions_root).await);

    // Backstop for incognito chats whose surface never got to discard them — a closed laptop, a
    // killed tab, a dropped connection. The WebUI deletes its own on the way out and that is what
    // runs almost every time; this is what makes "almost" not the end of the story, because an
    // incognito transcript sitting in daemon RAM until the next restart is exactly the thing the mode
    // promises not to do. Nothing here touches the disk: an ephemeral session has no file to remove.
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

    // Goal session hub first — the **one** execution engine (one-execution-engine plan E3/E4).
    // Life-ops demo always; coding when a provider is available; dispatch pack so cron/webhook/
    // delegate are hosted sessions, not a second engine. Built before chat so `delegate` can use it.
    let mut goals_hub = liberado_session::GoalSessionHub::new(SessionStore::clone(&sessions));
    goals_hub.register_pack(Arc::new(liberado_session::LifeOpsDemoRunner));
    // Coding pack: hold Arc so we can attach the hub after `Arc::new` (S6 child goal sessions).
    let coding_pack: Option<Arc<liberado_coder_agent::CodingSessionPack>> =
        provider.as_ref().map(|p| {
            let work_parent = liberado_bootstrap::data_dir().join("goal-workspaces");
            let _ = std::fs::create_dir_all(&work_parent);
            {
                let mut pack = liberado_coder_agent::CodingSessionPack::new(p.clone(), work_parent)
                    .with_max_concurrent_coding_subagents(
                        config.tuning.dispatch.max_concurrent_coding_subagents,
                    );
                // A malformed `[coder]` section used to be swallowed by `if let Ok(..)`, so the
                // pack silently kept its defaults and the operator's settings did nothing — with
                // no line anywhere saying why. Cost an hour of "why is my configured model being
                // ignored"; say it out loud instead.
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
                if let Some(factory) =
                    liberado_bootstrap::CoderRoleProviderFactory::for_config(&config)
                {
                    pack = pack.with_provider_factory(std::sync::Arc::new(factory));
                }
                Arc::new(pack)
            }
        });
    if let Some(pack) = coding_pack.as_ref() {
        goals_hub.register_pack(Arc::clone(pack) as Arc<dyn liberado_session::DomainPackRunner>);
    }
    if let Some(pack) = liberado_bootstrap::build_dispatch_pack(
        &providers,
        &config,
        capability_catalog.clone(),
        mcp_registry.clone(),
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
    // S6: coding fan-out spawns child goal sessions on this same hub.
    if let Some(pack) = coding_pack.as_ref() {
        pack.attach_hub(Arc::clone(&goals));
        info!(
            max_concurrent = config.tuning.dispatch.max_concurrent_coding_subagents,
            "coding pack: hub attached for subagent fan-out"
        );
    }

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
            info!(
                "cron delivery: folding briefs into the sticky Telegram chat (quiet-delay defer)"
            );
            daemon.with_notifier(Arc::new(cdn))
        }
        _ => daemon,
    };

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
        // The same ledger the daemon reads. A tap is the authenticated act; the vault note it also
        // updates is only the human-readable view of a decision recorded here.
        .map(|b| {
            b.with_approval_ledger(liberado_common::ApprovalLedger::new(
                liberado_config::data_dir(),
            ))
        })
    {
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
                .with_activity_tracker(telegram_activity.clone())
                .with_command_menu(command_menu);
            info!(
                "Telegram free-form chat surface attached (slash commands enabled + menu registered)"
            );
        }
        tokio::spawn(bot.run());
    }

    let reaction_tx = state.reaction_tx();
    let daemon_handle = tokio::spawn(async move {
        daemon.run(reaction_tx).await.ok();
    });

    // Work-starting routes only: middleware refuses with `shutting_down` once drain begins.
    // Attach/cancel/park/list stay on the main router so clients can rejoin or stop work already
    // in flight. `POST /api/goals` is gated here for the same reason as chat — the gate is the
    // capability, not the surface that happened to be wired first.
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

    let app = Router::new()
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
        // Compression is scoped to the static fallback, deliberately not applied to the router as a
        // whole. The payload that needs it is the release .wasm (multi-MB, ~4x compressible, and the
        // whole page blocks on it over the tailnet); the payload that must never be buffered is
        // `/api/chat/stream`, where holding bytes back turns a live turn into a frozen UI. Scoping it
        // here makes that impossible by construction rather than by trusting a predicate.
        .fallback_service(
            tower::ServiceBuilder::new()
                .layer(CompressionLayer::new())
                .service(ServeDir::new(dist_dir())),
        );

    let addr = format!("0.0.0.0:{port}");
    let grace = shutdown::shutdown_grace_from_env();
    info!("Web UI server listening on http://{}", addr);
    info!(
        grace_secs = grace.as_secs(),
        "shutdown: on SIGTERM/Ctrl+C drain refuses new turns for up to grace_secs then exits \
         (set LIBERADO_SHUTDOWN_GRACE_SECS; compose stop_grace_period should be ≥ this)"
    );
    info!("API endpoints:");
    info!("  GET /api/status  — daemon status");
    info!("  GET /api/models  — live provider model catalog");
    info!("  POST /api/models/select  — hot-swap active model");
    info!("  GET /api/reactions?limit=20  — recent reactions");
    info!("  GET /api/vault  — vault info");
    info!("  GET /api/goals  — list goal sessions; POST /api/goals starts one (drain-gated)");
    info!("  GET /api/goals/{{id}}/stream  — SSE goal session events");
    info!("  /  — static frontend (build with `dx build` from crates/webui/)");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
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

    // Vault-watch / reaction loop: stop after chat drain so cron/vault reactions do not keep the
    // process alive past the grace budget.
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

/// Answer "would this write be allowed, and if not, which guard stops it?" — statically, from
/// config alone, without running anything.
///
/// # Why this exists
///
/// A tool call passes several independent guards, and until now **every one of them could say no
/// and none of them could say "it was me"**. Worse, a refusal and a deliberately-protected zone
/// produce the identical observable: a proposal. So a missing grant, a misdeclared MCP, and a
/// working policy are indistinguishable from outside — which is how a capability bug that denied
/// every subagent write survived months of use while the daemon logged that the grant was present.
///
/// `authority_decision` fixed that at runtime; this answers the same question *before* you deploy,
/// which is the difference between "run it and read the logs" and "ask".
///
/// Prints every guard's verdict rather than stopping at the first failure — the first `no` is
/// rarely the only one, and fixing them one deploy at a time is the slow path.
pub fn explain_write(
    dir: Option<&Path>,
    component: &str,
    qualified_tool: &str,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use liberado_common::{Capability, WriteTarget, bare_tool_name, mcp_of};

    let resolved = dir
        .map(Path::to_path_buf)
        .or_else(liberado_bootstrap::config_dir);
    let (config, _) = liberado_bootstrap::load_config(resolved.as_deref())?;

    let mcp_name = mcp_of(qualified_tool);
    let bare = bare_tool_name(qualified_tool);
    let caps = config.policy.capabilities_for(component);
    // The same descriptor snapshot the live catalog is seeded from at boot, so this answers with
    // exactly the declarations the daemon would enforce — not a re-derivation that could disagree.
    let catalog = liberado_config::catalog_from_config(&config);

    println!("would `{component}` be allowed to call `{qualified_tool}` on `{path}`?\n");

    let mut blockers: Vec<String> = Vec::new();
    let say = |ok: bool| if ok { "PASS" } else { "BLOCK" };

    // 1. Is the MCP even declared, and granted?
    let Some(descriptor) = catalog.iter().find(|d| d.name == mcp_name).cloned() else {
        println!("  [BLOCK] mcp_declared      '{mcp_name}' is not an enabled [[mcps]] entry");
        println!("\nverdict: BLOCKED — the MCP does not exist in this config.");
        return Ok(());
    };
    // `grants_tool`, because this explainer was asked about a *specific* tool and is echoed to a human
    // as a verdict. `grants_mcp` answers "is this MCP reachable at all", which for a partial grant is
    // true even when the named tool is not granted — an explainer that reports PASS on a call the
    // runtime would refuse is worse than no explainer.
    let granted = caps.grants_tool(qualified_tool);
    let needed = if caps.grants_mcp(mcp_name) {
        format!("ExecuteTool(\"{qualified_tool}\")")
    } else {
        format!("ExecuteMcp(\"{mcp_name}\") or ExecuteTool(\"{qualified_tool}\")")
    };
    println!("  [{}] mcp_grant         needed {needed}", say(granted));
    if !granted {
        blockers.push(format!(
            "add {{ ExecuteTool = \"{qualified_tool}\" }} (or {{ ExecuteMcp = \"{mcp_name}\" }} for \
             the whole server) to the '{component}' grant in policy.toml"
        ));
    }

    // 2. What does this call write, per the MCP's own declaration + these arguments?
    let args = serde_json::json!({ "path": path });
    let target = liberado_common::write_target(&descriptor, bare, &args);
    let zone = match &target {
        WriteTarget::NotAWrite => {
            println!(
                "  [PASS] write_target      '{bare}' is a read on this MCP — no write guards apply"
            );
            println!(
                "\nverdict: {}",
                if blockers.is_empty() {
                    "ALLOWED"
                } else {
                    "BLOCKED"
                }
            );
            for b in &blockers {
                println!("  fix: {b}");
            }
            return Ok(());
        }
        WriteTarget::Undeterminable(why) => {
            println!("  [BLOCK] write_target      cannot place this write: {why}");
            blockers.push(
                "give the path a leading zone segment, or declare zone_from_arg/write_tools"
                    .to_string(),
            );
            println!("\nverdict: BLOCKED");
            for b in &blockers {
                println!("  fix: {b}");
            }
            return Ok(());
        }
        WriteTarget::Zone(z) => z.clone(),
    };
    println!("  [PASS] write_target      resolves to zone '{zone}'");

    // 3. Does the component hold Write on that zone?
    let holds_write = caps.contains(&Capability::Write(liberado_common::Zone::vault(&zone)));
    println!(
        "  [{}] write_capability  needed Write(Vault(\"{zone}\"))",
        say(holds_write)
    );
    if !holds_write {
        blockers.push(format!(
            "add {{ Write = {{ Vault = \"{zone}\" }} }} to the '{component}' grant"
        ));
    }

    // 4. Is the zone itself directly agent-writable?
    let class = config.policy.write_class(&zone);
    let class_ok = class.allows_direct_agent_write();
    println!(
        "  [{}] zone_write_class  zone '{zone}' is {class:?}{}",
        say(class_ok),
        if config.policy.zones.iter().any(|z| z.zone == zone) {
            ""
        } else {
            " (UNDECLARED — fail-safe default)"
        }
    );
    if !class_ok {
        blockers.push(format!(
            "declare zone '{zone}' with write_class = \"agent_writable\" in policy.toml \
             (undeclared zones default to proposal_only)"
        ));
    }

    // 5. Consequence — proposal-gated rather than refused, but still not a direct write.
    let consequence = descriptor.consequence;
    let conseq_ok = consequence < liberado_common::CONSEQUENCE_GATE;
    println!(
        "  [{}] consequence       '{mcp_name}' is {consequence:?} (gate is {:?})",
        say(conseq_ok),
        liberado_common::CONSEQUENCE_GATE
    );
    if !conseq_ok {
        blockers.push(format!(
            "'{mcp_name}' is rated {consequence:?}, so every call is proposal-gated by design"
        ));
    }

    if blockers.is_empty() {
        println!("\nverdict: ALLOWED — this write would execute directly.");
    } else {
        println!("\nverdict: BLOCKED by {} guard(s):", blockers.len());
        for b in &blockers {
            println!("  fix: {b}");
        }
    }
    Ok(())
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
    .with_live_catalog(catalog.clone())
    .with_dispatcher_capabilities(dispatcher_caps)
    .with_delegation_mode(main_agent_cfg.delegation_mode);

    // CH3 context compaction: config-tier knobs → kernel runtime type (per-model absolute
    // triggers + daemon default). See `state::compaction_config_for_face`. Summaries use the face
    // provider (docs/future-work/context-compaction-plan.md §Summary generation).
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
mod prompt_preview_tests {
    use super::*;
    use liberado_config::Config;

    /// A config with one `basic-chat`-shaped profile: no dispatch, a nudge, one whole-server grant
    /// and two named tools.
    fn config_with_basic_chat() -> Config {
        let toml = r#"
vault_path = "/tmp/vault"

[main_agent]
delegation_mode = true

# Declared because the loader refuses a profile naming an MCP that does not exist — the fail-closed
# check that makes "config names a tool the toolset lacks" unrepresentable here.
[[mcps]]
name = "liberado-search-orchestrator-mcp"
description = "search"
consequence = "read_only"
transport = { kind = "http", url = "http://search:8080" }

[[mcps]]
name = "turbovault"
description = "vault"
consequence = "read_only"
transport = { kind = "http", url = "http://turbovault:3001" }

[[session_profiles]]
name          = "basic-chat"
delegation    = false
prompt_append = "Answer directly and briefly."
read  = []
write = []
mcps  = [
  "liberado-search-orchestrator-mcp",
  { name = "turbovault", tools = ["tasks_list"] },
]
"#;
        // Written to a real directory and loaded through the real loader, rather than assembled
        // in memory: the point of this command is to report what the *config* produces, so a
        // fixture that skipped parsing could pass while the file it stands for failed to load.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("topology.toml"), toml).expect("write topology");
        let (config, _) = liberado_config::load_config(Some(dir.path())).expect("fixture config");
        config
    }

    /// The bug this command exists for, catchable **from config, with no daemon**.
    ///
    /// Live on 2026-07-28 a `basic-chat` session was handed the face-agent prompt — "you are a face
    /// agent, not a tool user… call the `delegate` tool" — while holding no `delegate`. It announced
    /// work and did none. Finding that cost a 17-minute build and a browserless run. This assertion
    /// is the same finding in milliseconds.
    #[test]
    fn a_non_delegating_profile_is_not_described_as_a_face_agent() {
        let config = config_with_basic_chat();
        let preview = compose_chat_prompt(&config, Some("basic-chat")).unwrap();

        assert!(!preview.delegation);
        assert_eq!(
            preview.system_messages[0],
            liberado_main_agent::DEFAULT_SYSTEM_PROMPT
        );
        assert!(
            !preview.system_messages[0].contains("delegate"),
            "a chat that cannot delegate must not be told to call `delegate`"
        );
        let manifest = preview.system_messages.last().unwrap();
        assert!(
            !manifest.contains("delegate"),
            "and `delegate` must not appear in its tool list either: {manifest}"
        );
    }

    /// The composed order must mirror what `ChatSessions` injects per turn — base, nudge, manifest.
    /// If these diverge the command reports a prompt nobody is ever given.
    #[test]
    fn the_composed_order_mirrors_the_turn() {
        let preview = compose_chat_prompt(&config_with_basic_chat(), Some("basic-chat")).unwrap();
        assert_eq!(preview.system_messages.len(), 3);
        assert_eq!(preview.system_messages[1], "Answer directly and briefly.");
        assert!(
            preview.system_messages[2].contains("available to you on this turn"),
            "the manifest must be last, as it is in the turn"
        );
    }

    /// Per-tool grants render exactly; whole-server grants cannot be resolved without a live daemon
    /// and must say so rather than pretending to be a resolved name.
    #[test]
    fn whole_server_grants_are_marked_rather_than_faked() {
        let preview = compose_chat_prompt(&config_with_basic_chat(), Some("basic-chat")).unwrap();
        let manifest = preview.system_messages.last().unwrap();

        assert!(manifest.contains("turbovault:tasks_list"), "{manifest}");
        assert!(
            manifest.contains("liberado-search-orchestrator-mcp:*"),
            "an unexpandable grant must be visibly unexpanded: {manifest}"
        );
        assert_eq!(
            preview.unresolved_mcps,
            vec!["liberado-search-orchestrator-mcp".to_string()],
            "and the caller must be told which names are approximate"
        );
    }

    /// A chat naming no profile inherits the daemon's delegation mode and gets `delegate` — the
    /// path every pre-existing conversation is on, so a regression here is the widest possible.
    #[test]
    fn no_profile_inherits_the_daemon_default() {
        let preview = compose_chat_prompt(&config_with_basic_chat(), None).unwrap();
        assert!(preview.delegation, "must inherit delegation_mode = true");
        assert_eq!(
            preview.system_messages[0],
            liberado_main_agent::HUMAN_INTERFACE_SYSTEM_PROMPT
        );
        assert!(
            preview
                .system_messages
                .last()
                .unwrap()
                .contains(liberado_main_agent::DELEGATE_TOOL_NAME),
            "a delegating chat's one tool is `delegate`"
        );
    }

    /// An unknown profile must be an error, not a silent fall-through to the default — the same
    /// rule the switching endpoint enforces, for the same reason: a typo resolving to "no profile"
    /// means quietly reporting the *wider* grant.
    #[test]
    fn an_unknown_profile_is_refused() {
        assert!(compose_chat_prompt(&config_with_basic_chat(), Some("nope")).is_err());
    }
}

#[cfg(test)]
mod startup_reconciliation_tests {
    /// F7 is a startup fix, not only a hub helper. Keep the production call after every pack is
    /// registered (so `can_resume` is authoritative) and before the hub is exposed to callers.
    #[test]
    fn run_reconciles_parked_sessions_after_pack_registration() {
        let source = include_str!("lib.rs");
        let run = source
            .split_once("pub async fn run(vault_path: String)")
            .and_then(|(_, tail)| tail.split_once("pub fn config_check("))
            .map(|(body, _)| body)
            .expect("server source must contain the production run body");

        let reconcile = run
            .find("goals_hub.reconcile_parked_at_startup().await")
            .expect("daemon startup must call parked-session reconciliation");
        for registration in [
            "goals_hub.register_pack(Arc::new(liberado_session::LifeOpsDemoRunner))",
            "goals_hub.register_pack(Arc::clone(pack)",
            "goals_hub.register_pack(Arc::new(pack))",
        ] {
            let registered = run
                .find(registration)
                .unwrap_or_else(|| panic!("missing production pack registration: {registration}"));
            assert!(
                registered < reconcile,
                "parked sessions must be classified only after all packs are registered"
            );
        }
        let exposed = run
            .find("let goals = Arc::new(goals_hub);")
            .expect("production hub must be exposed through Arc");
        assert!(
            reconcile < exposed,
            "startup reconciliation must finish before routes or workers can use the hub"
        );
    }
}
