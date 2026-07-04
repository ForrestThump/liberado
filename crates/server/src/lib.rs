//! # liberado-server
//!
//! The Liberado daemon process, as a library: it assembles the provider/chat/daemon (via
//! `liberado_bootstrap`), spawns the vault watch loop, and serves the HTTP/SSE API (`docs/reference/api.md`)
//! plus the built web frontend. The `liberado serve` subcommand calls [`run`]; this crate ships no
//! binary and does not init the global tracing subscriber (the CLI owns that, so a library embedding
//! us doesn't fight over it).

mod api;
mod state;

use std::sync::Arc;
use std::time::Instant;

use std::path::Path;

use axum::Router;
use liberado_common::{CapabilityCatalog, CapabilitySet, WriteProvenance};
use liberado_conversation_store::JsonlStore;
use liberado_daemon::Daemon;
use liberado_dispatcher::Dispatcher;
use liberado_executor::{Budget, Executor, ToolRuntime};
use liberado_main_agent::ChatSessions;
use liberado_mcp::McpRegistry;
use liberado_orchestrator::Orchestrator;
use liberado_provider::Provider;
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
    let provider = liberado_bootstrap::provider_from_env();
    let dispatcher_attached = provider.is_some();
    let model_name = provider.as_ref().map(|p| p.model().to_string());
    let mcp = liberado_bootstrap::mcp_registry_from_config(&config);
    let orchestrator_attached = dispatcher_attached && mcp.is_some();

    let (chat, chat_tools, chat_tool_names) = build_chat(
        provider.clone(),
        mcp,
        &config,
        capability_catalog.clone(),
        Path::new(&vault_path),
    )
    .await;

    let state = Arc::new(AppState {
        start_time: Instant::now(),
        reactions: Arc::new(Mutex::new(Vec::new())),
        dispatcher_attached,
        orchestrator_attached,
        vault_path: vault_path.clone(),
        chat,
        chat_tools,
        chat_tool_names,
        catalog: capability_catalog.clone(),
        model_name,
    });

    let daemon = Daemon::open("webui", &vault_path).await?;
    let daemon = liberado_bootstrap::configure_daemon(
        daemon,
        provider.as_ref(),
        &config,
        capability_catalog,
        Path::new(&vault_path),
    );

    let reaction_tx = state.reaction_tx();
    let daemon_handle = tokio::spawn(async move {
        daemon.run(reaction_tx).await.ok();
    });

    let app = Router::new()
        .route("/api/status", axum::routing::get(api::status))
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
            "/api/conversations/{id}",
            axum::routing::get(api::get_conversation).patch(api::patch_conversation_title),
        )
        .layer(CorsLayer::permissive())
        .with_state(state)
        .fallback_service(ServeDir::new(DIST_DIR));

    let addr = format!("0.0.0.0:{port}");
    info!("Web UI server listening on http://{}", addr);
    info!("API endpoints:");
    info!("  GET /api/status  — daemon status");
    info!("  GET /api/reactions?limit=20  — recent reactions");
    info!("  GET /api/vault  — vault info");
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
/// server, or none) + the executor + a durable conversation store. Returns `(chat, tool_count, tool_names)`
/// so callers can surface tool availability in diagnostics. Returns `(None, 0, empty)` when there's no
/// provider. This is the composition root — it injects the concrete [`JsonlStore`] so [`ChatSessions`]
/// stays store-agnostic.
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
) -> (Option<Arc<ChatSessions>>, usize, Vec<String>) {
    let provider = match provider {
        Some(p) => p,
        None => return (None, 0, Vec::new()),
    };

    // Capabilities granted to the "main-agent" component — chat's own tool-surface ceiling, and
    // (below) the same ceiling its owned Orchestrator scopes ExecuteDirect executions to. Computed
    // up front since both the Orchestrator construction and ChatSessions' guards need it.
    let capabilities = config.policy.capabilities_for("main-agent");

    // Conversation logs live outside the vault (Decision 12 operational data), under
    // `<LIBERADO_DATA_DIR>/conversations`. `JsonlStore::new` creates the directory.
    let store = Arc::new(JsonlStore::new(
        liberado_bootstrap::data_dir().join("conversations"),
    ));

    // Consequence catalog + proposals dir: shared with `configure_daemon`'s own Orchestrator wiring
    // (`liberado_bootstrap::guard_context`) so the two boot paths can't independently drift on how
    // either is derived. A one-time snapshot at boot is fine here — MCP declarations aren't
    // runtime-dynamic yet — but the dispatch-routing catalog below stays the live `Arc` so it and the
    // daemon/API never drift apart from each other.
    let guard = liberado_bootstrap::guard_context(&catalog, vault_path);
    let catalog_is_empty = guard.consequences.is_empty();

    let (runtime, orchestrator) = connect_chat_runtime(&provider, mcp, &capabilities, &guard).await;

    let tool_catalog = runtime.catalog();
    let tool_names: Vec<String> = tool_catalog.iter().map(|t| t.name.clone()).collect();
    let tool_count = tool_catalog.len();
    if tool_count > 0 {
        info!(count = tool_count, tools = ?tool_names, "chat: tool runtime ready");
    } else {
        info!("chat: no tools available — the model can only converse, not act");
    }

    // ── Build the guarded ChatSessions ───────────────────────────────────────
    let consequence_count = guard.consequences.len();
    let mut sessions = ChatSessions::new(
        store,
        Executor::new(provider.clone(), Budget::default()),
        runtime,
    )
    .with_guards(
        guard.consequences,
        capabilities,
        guard.proposals_dir,
        guard.signer.clone(),
    );

    if !catalog_is_empty {
        info!(
            count = consequence_count,
            "chat: runtime safety guards enabled (capability-scoped tools + RiskGatedToolRuntime)"
        );
    }

    // Dispatch routing (see `ChatSessions`' module docs): only when an orchestrator exists to
    // execute the non-`ExecuteDirect` outcomes. Mirrors `configure_daemon`'s dispatcher wiring.
    if let Some(orchestrator) = orchestrator {
        let dispatcher = Dispatcher::new(
            provider,
            config.tuning.dispatch.clone(),
            config.tuning.concurrency.max_reaction_depth,
        );
        info!("chat: dispatch routing enabled (Clarify/Propose/DispatchSubagent handled before execution)");
        sessions = sessions.with_dispatch(dispatcher, catalog, orchestrator);
    }

    (Some(Arc::new(sessions)), tool_count, tool_names)
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
                guard.proposals_dir.clone(),
                guard.signer.clone(),
            );
            (rt, Some(orchestrator))
        }
        None => {
            info!("chat: no MCP configured — chat will be conversation-only");
            (Arc::new(NoTools), None)
        }
    }
}
