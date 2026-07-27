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
mod latency;
mod state;
mod sticky;
mod telegram;

#[cfg(test)]
mod t1_conformance;

use std::sync::Arc;
use std::time::{Duration, Instant};

use std::path::Path;

use axum::Router;
use liberado_common::{CapabilityCatalog, WriteProvenance};
use liberado_daemon::Daemon;
use liberado_dispatcher::Dispatcher;
use liberado_executor::{Budget, Executor, ToolRuntime};
use liberado_main_agent::ChatSessions;
use liberado_mcp::McpRegistry;
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
    let goals = Arc::new(goals_hub);

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
        live_mcp: live_mcp.clone(),
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

    let app = Router::new()
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
        .route("/api/goals/{id}/park", axum::routing::post(api::goals_park))
        .layer(CorsLayer::permissive())
        .with_state(state)
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
    let granted = caps.grants_mcp(mcp_name);
    println!(
        "  [{}] mcp_grant         needed ExecuteMcp(\"{mcp_name}\")",
        say(granted)
    );
    if !granted {
        blockers.push(format!(
            "add {{ ExecuteMcp = \"{mcp_name}\" }} to the '{component}' grant in policy.toml"
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
    .with_live_catalog(catalog.clone())
    .with_dispatcher_capabilities(dispatcher_caps)
    .with_delegation_mode(main_agent_cfg.delegation_mode);

    // CH3 context compaction: config-tier knobs → the kernel's runtime type. Trigger is resolved
    // for the *face* model (per-model pct / absolute override, else global, else fallback) so a
    // 128k face model does not share a 48k default with a 64k one. Summaries use the face provider
    // (see docs/roadmap/context-compaction-plan.md §Summary generation).
    let compact = &main_agent_cfg.compaction;
    let face_model = provider.model();
    let trigger_tokens =
        compact.resolve_trigger_tokens(Some(face_model.as_str()), &config.topology.models);
    sessions = sessions.with_compaction(
        liberado_main_agent::CompactionConfig {
            enabled: compact.enabled,
            trigger_tokens,
            keep_recent_turns: compact.keep_recent_turns as usize,
            summary_max_tokens: compact.summary_max_tokens,
            tool_result_max_chars: compact.tool_result_max_chars as usize,
        },
        provider.clone(),
    );
    if compact.enabled {
        info!(
            face_model = %face_model,
            trigger_tokens,
            trigger_pct = compact.trigger_pct,
            keep_recent_turns = compact.keep_recent_turns,
            "chat: automatic context compaction enabled"
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
    let mut dispatcher = Dispatcher::new(
        dispatcher_provider,
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
