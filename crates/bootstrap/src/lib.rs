//! # liberado-bootstrap
//!
//! The **assembly** half of daemon composition: turns a loaded config (plus the env-sourced
//! provider) into a wired daemon — the dispatcher's capabilities + catalog and the MCP runtime
//! registry all come from `policy`/`topology`, while the inference provider still comes from the
//! environment (`DEEPSEEK_API_KEY`). Keeping daemon assembly in one place means the `cli` and server
//! binaries build the same daemon the same way, so the modes (watch-only / decide-only / act) can't
//! drift apart between them.
//!
//! The config **loader** itself (Decision 14 — resolve + merge the small per-section TOML files into
//! one validated `Config`) and the light path-resolution helpers built on it (`config_dir`,
//! `mcp_install_dir`, `data_dir`, `GuardContext`) live in `liberado-config` instead — this crate's
//! heavy assembly functions need `liberado-daemon`/`liberado-mcp`/`liberado-dispatcher`/
//! `liberado-orchestrator`/`liberado-provider-deepseek`, which a config-only consumer
//! (`liberado-mcp-forge`) has no use for. Re-exported here so `liberado-server`/`liberado-cli` see no
//! change from before this split.

pub use liberado_config::{
    Config, ConfigError, ConfigProvenance, GuardContext, capability_catalog_from_config,
    catalog_from_config, config_dir, data_dir, guard_context, load_config, mcp_install_dir,
};

use std::path::Path;
use std::sync::Arc;

use liberado_common::CapabilityCatalog;
use liberado_config::{McpTransport, managed_binary_path};
use liberado_daemon::Daemon;
use liberado_dispatcher::Dispatcher;
use liberado_mcp::{HttpConnector, McpRegistry, StdioConnector};
use liberado_orchestrator::Orchestrator;
use liberado_provider::Provider;
use liberado_provider_deepseek::DeepSeekProvider;

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
/// the loaded `config`, the shared, live `catalog` (built once by the caller via
/// [`capability_catalog_from_config`] and also handed to chat's own dispatch and the server's API —
/// one object, not three independent snapshots), and `vault_path` (the same resolved path `daemon`
/// itself was opened over — not re-derived from `config.topology.vault_path`, since a CLI override
/// can make those differ). With no provider the daemon stays watch-only; with a provider but no MCP
/// it is decide-only. This is the single owner of the daemon's decide/act wiring.
///
/// The dispatcher is built from `config.tuning`, and the `"dispatcher"` component's capabilities
/// (`config.policy.capabilities_for("dispatcher")` — the union of grants naming that component) are
/// its maximal authority, so the Decision 4 boundary is now *configured* rather than empty. The
/// same set also bounds the orchestrator's `ExecuteDirect` runtime, so a decision the dispatcher
/// approved can't reach an MCP outside what `"dispatcher"` was actually granted. The orchestrator's
/// MCP connection comes from `topology.mcps` too (single source with the catalog), so routing and
/// execution line up by name.
pub fn configure_daemon(
    daemon: Daemon,
    provider: Option<&Arc<dyn Provider>>,
    config: &Config,
    catalog: Arc<CapabilityCatalog>,
    vault_path: &Path,
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
    let capabilities = config.policy.capabilities_for("dispatcher");
    tracing::info!(
        grants = config.policy.grants.len(),
        capabilities = capabilities.capabilities.len(),
        "dispatcher capability boundary configured from policy"
    );
    // Runtime-level gating ingredients for the orchestrator's adaptive (non-seed) tool calls — the
    // same consequence catalog, vault-rooted proposals directory, and integrity signer chat's own
    // RiskGatedToolRuntime uses (see `RiskGatedToolRuntime`'s doc comment).
    let guard = guard_context(&catalog, &config.policy, vault_path);
    let daemon = daemon
        .with_dispatcher(dispatcher, catalog, capabilities.clone())
        .with_proposal_signer(guard.signer.clone())
        .with_zone_write_classes(guard.zone_write_classes.clone());
    match mcp_registry_from_config(config) {
        Some(factory) => {
            tracing::info!("orchestrator enabled (MCP execution)");
            daemon.with_orchestrator(Orchestrator::new(
                provider.clone(),
                factory,
                capabilities,
                guard.consequences,
                guard.zone_catalog,
                guard.zone_write_classes,
                guard.proposals_dir,
                guard.signer,
            ))
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
    use liberado_config::McpConfig;

    fn mcp(name: &str, enabled: bool, transport: McpTransport) -> McpConfig {
        McpConfig {
            name: name.into(),
            enabled,
            description: "test".into(),
            consequence: Consequence::Reversible,
            transport,
            default_zone: None,
            tools: Vec::new(),
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
