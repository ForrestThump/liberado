//! The `liberado` binary — the composition root: wires concrete implementations into the daemon and
//! runs the watch loop.
//!
//! Usage:
//!   liberado <vault-path>
//!   LIBERADO_VAULT=<vault-path> liberado
//!
//! Modes, by what's configured:
//!   - no `DEEPSEEK_API_KEY`               → watch-only (observe changes, no dispatch).
//!   - key set, no `LIBERADO_MCP_CMD`      → decide-only (dispatch decisions, no execution).
//!   - key + `LIBERADO_MCP_CMD` (+ `_ARGS`)→ act: dispatch decisions are executed against the MCP
//!     server spawned from that command (tool-mediated writes carry provenance and are loop-broken).
//!
//! Reactions are logged to stderr; stdout is left for data.

use std::sync::Arc;

use liberado_common::CapabilitySet;
use liberado_common::config::{ConcurrencyTuning, DispatchTuning};
use liberado_daemon::{Daemon, Reaction};
use liberado_dispatcher::Dispatcher;
use liberado_mcp::{StdioConnector, TurbomcpRuntimeFactory};
use liberado_orchestrator::Orchestrator;
use liberado_provider::Provider;
use liberado_provider_deepseek::DeepSeekProvider;
use tokio::sync::mpsc::unbounded_channel;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr) // logs to stderr (unbuffered); stdout stays for data
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let vault_path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("LIBERADO_VAULT").ok())
        .ok_or("usage: liberado <vault-path>  (or set LIBERADO_VAULT)")?;

    let daemon = Daemon::open("vault", &vault_path).await?;

    let daemon = match DeepSeekProvider::from_env() {
        Ok(provider) => {
            let provider: Arc<dyn Provider> = Arc::new(provider);
            tracing::info!(model = provider.model(), "dispatcher enabled (DeepSeek)");

            let dispatcher = Dispatcher::new(
                provider.clone(),
                DispatchTuning::default(),
                ConcurrencyTuning::default().max_reaction_depth,
            );
            let daemon = daemon.with_dispatcher(dispatcher, Vec::new(), CapabilitySet::empty());

            // Execution requires an MCP server to run tools against. With one configured, decisions
            // are orchestrated end-to-end; without one, the daemon decides but cannot act.
            attach_orchestrator(daemon, provider)
        }
        Err(_) => {
            tracing::warn!("DEEPSEEK_API_KEY not set — running watch-only (no dispatch)");
            daemon
        }
    };

    let (reactions, mut rx) = unbounded_channel::<Reaction>();
    tokio::spawn(async move {
        while let Some(reaction) = rx.recv().await {
            tracing::info!(
                event_type = %reaction.event.event_type,
                path = reaction.event.payload.path.as_deref().unwrap_or_default(),
                correlation_id = %reaction.event.correlation_id,
                outcome = reaction.outcome.label(),
                "REACTION"
            );
        }
    });

    daemon.run(reactions).await?;
    Ok(())
}

/// Attach an orchestrator (execution) when `LIBERADO_MCP_CMD` names an MCP server to spawn;
/// otherwise leave the daemon decide-only. `LIBERADO_MCP_ARGS` (optional, whitespace-separated)
/// supplies the server's arguments.
fn attach_orchestrator(daemon: Daemon, provider: Arc<dyn Provider>) -> Daemon {
    match std::env::var("LIBERADO_MCP_CMD") {
        Ok(command) if !command.trim().is_empty() => {
            let args = std::env::var("LIBERADO_MCP_ARGS")
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from)
                .collect();
            tracing::info!(%command, "orchestrator enabled (MCP execution)");
            let factory = TurbomcpRuntimeFactory::new(StdioConnector::new(command, args));
            daemon.with_orchestrator(Orchestrator::new(provider, factory))
        }
        _ => {
            tracing::warn!("LIBERADO_MCP_CMD not set — decide-only (no MCP execution)");
            daemon
        }
    }
}
