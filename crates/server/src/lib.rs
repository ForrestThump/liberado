//! # liberado-server
//!
//! The Liberado daemon process, as a library: it assembles the provider/chat/daemon (via
//! `liberado_bootstrap`), spawns the vault watch loop, and serves the HTTP/SSE API (`docs/interface.md`)
//! plus the built web frontend. The `liberado serve` subcommand calls [`run`]; this crate ships no
//! binary and does not init the global tracing subscriber (the CLI owns that, so a library embedding
//! us doesn't fight over it).

mod api;
mod state;

use std::sync::Arc;
use std::time::Instant;

use std::path::Path;

use axum::Router;
use liberado_bootstrap::catalog_from_config;
use liberado_common::{CapabilityCatalog, WriteProvenance};
use liberado_conversation_store::JsonlStore;
use liberado_daemon::Daemon;
use liberado_executor::{Budget, Executor, ToolRuntime};
use liberado_main_agent::ChatSessions;
use liberado_mcp::McpRegistry;
use liberado_orchestrator::RuntimeFactory;
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

    // Build the provider once and share it between the daemon (dispatch/execute) and chat.
    let provider = liberado_bootstrap::provider_from_env();
    let dispatcher_attached = provider.is_some();
    let mcp = liberado_bootstrap::mcp_registry_from_config(&config);
    let orchestrator_attached = dispatcher_attached && mcp.is_some();

    let (chat, chat_tools, chat_tool_names) = build_chat(provider.clone(), mcp, &config).await;

    // Build the capability catalog for API exposure.
    let capability_catalog = {
        let cat = CapabilityCatalog::new();
        for desc in catalog_from_config(&config) {
            cat.register(liberado_common::McpDescriptor {
                name: desc.name.clone(),
                description: desc.description.clone(),
                consequence: desc.consequence,
                provenance: None,
            });
        }
        Arc::new(cat)
    };

    let state = Arc::new(AppState {
        start_time: Instant::now(),
        reactions: Arc::new(Mutex::new(Vec::new())),
        dispatcher_attached,
        orchestrator_attached,
        vault_path: vault_path.clone(),
        chat,
        chat_tools,
        chat_tool_names,
        catalog: capability_catalog,
    });

    let daemon = Daemon::open("webui", &vault_path).await?;
    let daemon = liberado_bootstrap::configure_daemon(daemon, provider.as_ref(), &config);

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
            axum::routing::get(api::get_conversation),
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
/// When guard configuration (catalog, capabilities) is present, ChatSessions is configured with
/// the tool-advisor and RiskGatedToolRuntime for every turn.
async fn build_chat(
    provider: Option<Arc<dyn Provider>>,
    mcp: Option<McpRegistry>,
    config: &liberado_common::config::Config,
) -> (Option<Arc<ChatSessions>>, usize, Vec<String>) {
    let provider = match provider {
        Some(p) => p,
        None => return (None, 0, Vec::new()),
    };

    // Connect a tool runtime once, reused for the chat's lifetime. Without an MCP, chat still works
    // as plain conversation.
    let runtime: Arc<dyn ToolRuntime> = match mcp {
        Some(registry) => {
            let provenance = WriteProvenance::agent("liberado-chat", "chat-session");
            match registry.runtime_for(&[], provenance).await {
                Ok(rt) => {
                    info!("chat: connected MCP tools");
                    Arc::from(rt)
                }
                Err(e) => {
                    warn!(error = %e, "chat: MCP connect failed — continuing without tools");
                    Arc::new(NoTools)
                }
            }
        }
        None => {
            info!("chat: no MCP configured — chat will be conversation-only");
            Arc::new(NoTools)
        }
    };

    let catalog = runtime.catalog();
    let tool_names: Vec<String> = catalog.iter().map(|t| t.name.clone()).collect();
    let tool_count = catalog.len();
    if tool_count > 0 {
        info!(count = tool_count, tools = ?tool_names, "chat: tool runtime ready");
    } else {
        info!("chat: no tools available — the model can only converse, not act");
    }

    // Conversation logs live outside the vault (Decision 12 operational data), under
    // `<LIBERADO_DATA_DIR>/conversations`. `JsonlStore::new` creates the directory.
    let data_dir = std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into());
    let store = Arc::new(JsonlStore::new(Path::new(&data_dir).join("conversations")));

    // ── Build the guarded ChatSessions ───────────────────────────────────────
    //
    // Extract the MCP catalog from the config for consequence gating.
    let dispatcher_catalog = catalog_from_config(config);

    // Consequence catalog: (name, Consequence) pairs for risk gating.
    let consequences: Vec<(String, liberado_common::Consequence)> = dispatcher_catalog
        .iter()
        .map(|d| (d.name.clone(), d.consequence))
        .collect();

    // Base capabilities from policy grants.
    let capabilities = config.policy.base_capabilities();

    // Proposal files directory.
    let proposals_dir = Path::new(&data_dir).join("proposals");

    let sessions = ChatSessions::new(store, Executor::new(provider, Budget::default()), runtime)
        .with_guards(consequences, capabilities, proposals_dir);

    if !dispatcher_catalog.is_empty() {
        info!(
            count = dispatcher_catalog.len(),
            "chat: runtime safety guards enabled (capability-scoped tools + RiskGatedToolRuntime)"
        );
    }

    (Some(Arc::new(sessions)), tool_count, tool_names)
}
