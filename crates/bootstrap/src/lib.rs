//! # liberado-bootstrap
//!
//! Composition helpers that build Liberado's components, shared by every binary that assembles a
//! daemon. Two things live here: the config **loader** (Decision 14 — resolve + merge the small
//! per-section TOML files into one validated [`Config`]) and the **assembly** that turns that config
//! (plus the env-sourced provider) into a wired daemon — the dispatcher's capabilities + catalog and
//! the MCP runtime registry all come from `policy`/`topology`, while the inference provider still
//! comes from the environment (`DEEPSEEK_API_KEY`). Keeping daemon assembly in one place means the
//! `cli` and server binaries build the same daemon the same way, so the modes (watch-only /
//! decide-only / act) can't drift apart between them.

mod config;

pub use config::{ConfigError, ConfigProvenance, catalog_from_config, config_dir, load_config};

use std::path::PathBuf;
use std::sync::Arc;

use liberado_common::config::{Config, McpTransport, managed_binary_path};
use liberado_daemon::Daemon;
use liberado_dispatcher::Dispatcher;
use liberado_mcp::{HttpConnector, McpRegistry, StdioConnector};
use liberado_orchestrator::Orchestrator;
use liberado_provider::Provider;
use liberado_provider_deepseek::DeepSeekProvider;

/// Where `liberado-mcp-forge` installs managed MCP binaries, and where
/// [`McpTransport::Managed`] resolution looks for them (Decision: convention over mutation —
/// `topology.toml` never gets a file path written into it; a `name` resolves to a path by this
/// one rule, on both the forge tool's and the daemon's side).
///
/// 1. `LIBERADO_MCP_INSTALL_DIR` env var — explicit intent, always wins.
/// 2. Platform data dir (`dirs::data_dir()/liberado/mcp-bin`), mirroring how [`config_dir`]
///    resolves `LIBERADO_CONFIG_DIR`.
pub fn mcp_install_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LIBERADO_MCP_INSTALL_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("liberado")
        .join("mcp-bin")
}

/// Build the shared inference provider from the environment (`DEEPSEEK_API_KEY`). `None` means no key
/// is set, so the daemon runs watch-only and chat is disabled.
pub fn provider_from_env() -> Option<Arc<dyn Provider>> {
    match DeepSeekProvider::from_env() {
        Ok(provider) => {
            let provider: Arc<dyn Provider> = Arc::new(provider);
            tracing::info!(model = provider.model(), "provider configured (DeepSeek)");
            Some(provider)
        }
        Err(_) => None,
    }
}

/// Build the MCP registry from the ENABLED MCPs in `config.topology.mcps`, registering each by its
/// `name` with a connector from its transport. `None` when no MCP is enabled (decide-only daemon,
/// tool-less chat). This shares ONE source (topology.mcps) with the dispatcher catalog, so a name the
/// dispatcher routes to is a name the runtime can actually connect to.
pub fn mcp_registry_from_config(config: &Config) -> Option<McpRegistry> {
    let registry = config.topology.mcps.iter().filter(|m| m.enabled).fold(
        McpRegistry::new(),
        |registry, m| match &m.transport {
            McpTransport::Stdio { command, args } => {
                registry.register(&m.name, StdioConnector::new(command.clone(), args.clone()))
            }
            McpTransport::Http { url } => {
                registry.register(&m.name, HttpConnector::new(url.clone()))
            }
            McpTransport::Managed => {
                let bin = managed_binary_path(&mcp_install_dir(), &m.name);
                registry.register(&m.name, StdioConnector::new(bin.to_string_lossy(), vec![]))
            }
        },
    );
    (!registry.is_empty()).then_some(registry)
}

/// Attach the dispatcher and (when an MCP server is configured) the orchestrator to `daemon`, using
/// the loaded `config`. With no provider the daemon stays watch-only; with a provider but no MCP it
/// is decide-only. This is the single owner of the daemon's decide/act wiring.
///
/// The dispatcher is built from `config.tuning` and — crucially — holds `config.policy`'s base
/// capabilities (the union of every grant) as its maximal authority, so the Decision 4 boundary is
/// now *configured* rather than empty. Both the dispatcher catalog and the orchestrator's MCP
/// connection come from `topology.mcps` (single source), so routing and execution line up by name.
pub fn configure_daemon(
    daemon: Daemon,
    provider: Option<&Arc<dyn Provider>>,
    config: &Config,
) -> Daemon {
    let Some(provider) = provider else {
        tracing::warn!("DEEPSEEK_API_KEY not set — running watch-only (no dispatch)");
        return daemon;
    };
    let dispatcher = Dispatcher::new(
        provider.clone(),
        config.tuning.dispatch.clone(),
        config.tuning.concurrency.max_reaction_depth,
    );
    let capabilities = config.policy.base_capabilities();
    tracing::info!(
        grants = config.policy.grants.len(),
        capabilities = capabilities.capabilities.len(),
        "dispatcher capability boundary configured from policy"
    );
    // Catalog AND connection both derive from `topology.mcps` now (single source): the dispatcher
    // routes over the enabled MCPs and the orchestrator connects to those same names, so a routed
    // name is always a name the runtime can reach.
    let catalog = catalog_from_config(config);
    let daemon = daemon.with_dispatcher(dispatcher, catalog, capabilities);
    match mcp_registry_from_config(config) {
        Some(factory) => {
            tracing::info!("orchestrator enabled (MCP execution)");
            daemon.with_orchestrator(Orchestrator::new(provider.clone(), factory))
        }
        None => {
            tracing::warn!("no enabled MCP in topology.mcps — decide-only (no MCP execution)");
            daemon
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::capability::Consequence;
    use liberado_common::config::McpConfig;

    fn mcp(name: &str, enabled: bool, transport: McpTransport) -> McpConfig {
        McpConfig {
            name: name.into(),
            enabled,
            description: "test".into(),
            consequence: Consequence::Reversible,
            transport,
        }
    }

    #[test]
    fn registry_registers_each_enabled_mcp_by_name() {
        let mut config = Config::default();
        config.topology.mcps = vec![
            mcp(
                "tasks-mcp",
                true,
                McpTransport::Stdio {
                    command: "npx".into(),
                    args: vec!["-y".into(), "@scope/tasks".into()],
                },
            ),
            mcp(
                "wiki-mcp",
                true,
                McpTransport::Http {
                    url: "https://mcp.deepwiki.com/mcp".into(),
                },
            ),
            // Disabled => must not be registered, so the dispatcher can't route to a dead name.
            mcp(
                "email-mcp",
                false,
                McpTransport::Stdio {
                    command: "email-mcp".into(),
                    args: vec![],
                },
            ),
        ];

        let registry = mcp_registry_from_config(&config).expect("two enabled MCPs => Some");
        let mut names: Vec<&str> = registry.names().collect();
        names.sort_unstable();
        assert_eq!(names, vec!["tasks-mcp", "wiki-mcp"]);
    }

    #[test]
    fn managed_transport_registers_by_name_too() {
        let mut config = Config::default();
        config.topology.mcps = vec![mcp("weather-mcp", true, McpTransport::Managed)];

        let registry = mcp_registry_from_config(&config).expect("one enabled MCP => Some");
        let names: Vec<&str> = registry.names().collect();
        assert_eq!(names, vec!["weather-mcp"]);
    }

    #[test]
    fn no_enabled_mcp_yields_none() {
        let mut config = Config::default();
        config.topology.mcps = vec![mcp(
            "email-mcp",
            false,
            McpTransport::Stdio {
                command: "email-mcp".into(),
                args: vec![],
            },
        )];
        assert!(mcp_registry_from_config(&config).is_none());
    }
}
