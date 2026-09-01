//! Desired MCP peer set → live catalog + registry transition (boot and hot-reload).
//!
//! One path for both: build connectors from topology MCP slice, validate, then apply so
//! [`CapabilityCatalog`] routing and [`McpRegistry`] acquisition stay consistent. A failed
//! validation leaves the previous live set untouched.
//!
//! This is the extension seam for a future programmatic peer source — feed the same apply with a
//! constructed desired list. **Not** agent self-registration of MCPs.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use liberado_common::{CapabilityCatalog, McpDescriptor};
use liberado_config::{
    Config, McpConfig, McpTransport, config_dir, managed_binary_path, mcp_install_dir,
};
use liberado_mcp::{HttpConnector, McpConnector, McpRegistry, StdioConnector};

use crate::docker_argv;

/// Outcome of a successful peer-set apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpApplyReport {
    /// Peer names present after apply (enabled topology MCPs).
    pub enabled: Vec<String>,
    /// Names removed from the previous live set.
    pub removed: Vec<String>,
    /// Names newly added relative to the previous live set.
    pub added: Vec<String>,
}

/// Why an apply was rejected (previous live set is unchanged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpApplyError {
    pub message: String,
}

impl std::fmt::Display for McpApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for McpApplyError {}

/// Live catalog + shared registry handle for boot and operator hot-reload.
///
/// Clones of [`McpRegistry`] share the same connector map, so daemon pools and chat stay in sync
/// after [`LiveMcpController::apply_config`].
#[derive(Clone)]
pub struct LiveMcpController {
    catalog: Arc<CapabilityCatalog>,
    registry: McpRegistry,
}

impl LiveMcpController {
    /// Wrap an existing catalog + registry (typically after first boot apply).
    pub fn new(catalog: Arc<CapabilityCatalog>, registry: McpRegistry) -> Self {
        Self { catalog, registry }
    }

    /// Empty catalog + empty registry (tests / decide-only fixtures).
    pub fn empty() -> Self {
        let catalog = Arc::new(CapabilityCatalog::new());
        let registry = McpRegistry::new().with_health_catalog(catalog.clone());
        Self { catalog, registry }
    }

    pub fn catalog(&self) -> Arc<CapabilityCatalog> {
        self.catalog.clone()
    }

    /// Shared live registry (cheap clone).
    pub fn registry(&self) -> McpRegistry {
        self.registry.clone()
    }

    /// Apply the enabled MCP slice of `config` to catalog + registry.
    pub fn apply_config(&self, config: &Config) -> Result<McpApplyReport, McpApplyError> {
        apply_mcp_peer_set(&self.catalog, &self.registry, &config.topology.mcps)
    }

    /// Reload topology MCP peers from the process config dir (`load_config`), validate whole
    /// config, then apply only the MCP peer set. Other config sections are re-read for validation
    /// but not hot-applied (out of scope).
    ///
    /// The config dir is resolved here (not at the call site) because `load_config(None)` means
    /// "all defaults", not "resolve the config dir": passing `None` loads an empty topology with
    /// no `vault_path`, which fails validation with `topology.vault_path is required` and turns
    /// every reload into a 400 that silently keeps the stale peer set.
    pub fn reload_from_config_dir(&self) -> Result<McpApplyReport, McpApplyError> {
        self.reload_from_dir(config_dir().as_deref())
    }

    /// Core of [`Self::reload_from_config_dir`], taking the resolved config dir explicitly so the
    /// load/validate/apply path is testable without process-global env state. `None` is the same
    /// "all defaults" contract `load_config` documents — and therefore the same validation failure
    /// (no `vault_path`), which is exactly the bug this seam exists to make visible.
    fn reload_from_dir(&self, dir: Option<&Path>) -> Result<McpApplyReport, McpApplyError> {
        let (config, _prov) = liberado_config::load_config(dir).map_err(|e| McpApplyError {
            message: format!("reload: failed to load config: {e}"),
        })?;
        config.validate().map_err(|e| McpApplyError {
            message: format!("reload: config validation failed: {e}"),
        })?;
        self.apply_config(&config)
    }
}

/// Sync catalog + registry to the **enabled** entries in `mcps` (desired peer set).
///
/// Disabled entries are omitted (same as boot). On validation failure, neither catalog nor
/// registry is mutated.
pub fn apply_mcp_peer_set(
    catalog: &CapabilityCatalog,
    registry: &McpRegistry,
    mcps: &[McpConfig],
) -> Result<McpApplyReport, McpApplyError> {
    validate_desired_mcps(mcps)?;

    let desired: Vec<&McpConfig> = mcps.iter().filter(|m| m.enabled).collect();
    let desired_names: HashSet<String> = desired.iter().map(|m| m.name.clone()).collect();

    // Build the full connector map **before** mutating live state.
    let connectors = build_connectors(&desired)?;

    let previous: HashSet<String> = registry.names().into_iter().collect();
    let added: Vec<String> = desired_names.difference(&previous).cloned().collect();
    let removed: Vec<String> = previous.difference(&desired_names).cloned().collect();

    // Swap runtime peers first, then catalog: never leave a name routable without a connector.
    // During the brief window a name may be connectable but not yet in routing (safer than the
    // reverse). After both complete, they match.
    registry.replace_connectors(connectors);
    sync_catalog_to_desired(catalog, &desired);

    let mut enabled: Vec<String> = desired_names.into_iter().collect();
    enabled.sort();
    let mut added = added;
    let mut removed = removed;
    added.sort();
    removed.sort();

    tracing::info!(
        enabled = enabled.len(),
        added = added.len(),
        removed = removed.len(),
        "MCP peer set applied"
    );

    Ok(McpApplyReport {
        enabled,
        removed,
        added,
    })
}

fn validate_desired_mcps(mcps: &[McpConfig]) -> Result<(), McpApplyError> {
    let mut seen = HashSet::new();
    for m in mcps {
        if m.name.trim().is_empty() {
            return Err(McpApplyError {
                message: "MCP peer set rejected: empty MCP name".into(),
            });
        }
        if !seen.insert(m.name.clone()) {
            return Err(McpApplyError {
                message: format!("MCP peer set rejected: duplicate MCP name '{}'", m.name),
            });
        }
        if m.enabled {
            match &m.transport {
                McpTransport::Stdio { command, .. } if command.trim().is_empty() => {
                    return Err(McpApplyError {
                        message: format!(
                            "MCP peer set rejected: MCP '{}' has empty stdio command",
                            m.name
                        ),
                    });
                }
                McpTransport::Http { url } if url.trim().is_empty() => {
                    return Err(McpApplyError {
                        message: format!(
                            "MCP peer set rejected: MCP '{}' has empty HTTP url",
                            m.name
                        ),
                    });
                }
                McpTransport::Docker { image, .. } if image.trim().is_empty() => {
                    return Err(McpApplyError {
                        message: format!(
                            "MCP peer set rejected: MCP '{}' has empty docker image",
                            m.name
                        ),
                    });
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn build_connectors(
    desired: &[&McpConfig],
) -> Result<HashMap<String, Arc<dyn McpConnector>>, McpApplyError> {
    let mut map = HashMap::with_capacity(desired.len());
    for m in desired {
        let connector: Arc<dyn McpConnector> = match &m.transport {
            McpTransport::Stdio { command, args } => {
                Arc::new(StdioConnector::new(command.clone(), args.clone()))
            }
            McpTransport::Http { url } => Arc::new(HttpConnector::new(url.clone())),
            McpTransport::Managed => {
                let bin = managed_binary_path(&mcp_install_dir(), &m.name);
                Arc::new(StdioConnector::new(bin.to_string_lossy(), vec![]))
            }
            McpTransport::Docker {
                image,
                command,
                args,
                volumes,
                env,
            } => {
                let argv = docker_argv(image, command.as_deref(), args, volumes, env);
                Arc::new(StdioConnector::new("docker", argv))
            }
        };
        map.insert(m.name.clone(), connector);
    }
    Ok(map)
}

fn descriptor_from_mcp(m: &McpConfig) -> McpDescriptor {
    McpDescriptor {
        name: m.name.clone(),
        description: m.description.clone(),
        consequence: m.consequence,
        provenance: None,
        default_zone: m.default_zone.clone(),
        tool_zones: m
            .tools
            .iter()
            .map(|t| (t.name.clone(), t.zone.clone()))
            .collect(),
        zone_from_arg: m.zone_from_arg.clone(),
        write_tools: m.write_tools.clone(),
    }
}

fn sync_catalog_to_desired(catalog: &CapabilityCatalog, desired: &[&McpConfig]) {
    let target: HashSet<String> = desired.iter().map(|m| m.name.clone()).collect();
    for existing in catalog.descriptors() {
        if !target.contains(&existing.name) {
            catalog.deregister(&existing.name);
        }
    }
    for m in desired {
        catalog.register(descriptor_from_mcp(m));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_config::McpTransport;

    fn mcp(name: &str, enabled: bool, transport: McpTransport) -> McpConfig {
        McpConfig {
            name: name.into(),
            enabled,
            description: format!("{name} desc"),
            consequence: liberado_common::Consequence::Reversible,
            transport,
            default_zone: None,
            tools: vec![],
            zone_from_arg: None,
            write_tools: vec![],
            writes_vault: Some(false),
        }
    }

    fn stdio(name: &str, enabled: bool) -> McpConfig {
        mcp(
            name,
            enabled,
            McpTransport::Stdio {
                command: format!("{name}-bin"),
                args: vec![],
            },
        )
    }

    #[test]
    fn apply_enables_and_disables_peers_on_catalog_and_registry() {
        let catalog = Arc::new(CapabilityCatalog::new());
        let registry = McpRegistry::new().with_health_catalog(catalog.clone());

        let first = vec![stdio("tasks", true), stdio("weather", true)];
        let report = apply_mcp_peer_set(&catalog, &registry, &first).unwrap();
        assert_eq!(
            report.enabled,
            vec!["tasks".to_string(), "weather".to_string()]
        );
        assert_eq!(
            report.added,
            vec!["tasks".to_string(), "weather".to_string()]
        );
        assert!(report.removed.is_empty());

        let mut routing: Vec<_> = catalog
            .routing_descriptors()
            .into_iter()
            .map(|d| d.name)
            .collect();
        routing.sort();
        assert_eq!(routing, vec!["tasks", "weather"]);
        let mut names = registry.names();
        names.sort();
        assert_eq!(names, vec!["tasks", "weather"]);

        // Disable weather, add memory.
        let second = vec![
            stdio("tasks", true),
            stdio("weather", false),
            stdio("memory", true),
        ];
        let report = apply_mcp_peer_set(&catalog, &registry, &second).unwrap();
        assert_eq!(
            report.enabled,
            vec!["memory".to_string(), "tasks".to_string()]
        );
        assert_eq!(report.added, vec!["memory".to_string()]);
        assert_eq!(report.removed, vec!["weather".to_string()]);

        let mut routing: Vec<_> = catalog
            .routing_descriptors()
            .into_iter()
            .map(|d| d.name)
            .collect();
        routing.sort();
        assert_eq!(routing, vec!["memory", "tasks"]);
        let mut names = registry.names();
        names.sort();
        assert_eq!(names, vec!["memory", "tasks"]);

        // Full catalog must not still list weather.
        let full: HashSet<_> = catalog.descriptors().into_iter().map(|d| d.name).collect();
        assert!(!full.contains("weather"));
        assert!(full.contains("tasks") && full.contains("memory"));
    }

    #[test]
    fn reject_duplicate_leaves_prior_set() {
        let catalog = Arc::new(CapabilityCatalog::new());
        let registry = McpRegistry::new().with_health_catalog(catalog.clone());
        apply_mcp_peer_set(&catalog, &registry, &[stdio("tasks", true)]).unwrap();

        let bad = vec![
            stdio("tasks", true),
            stdio("tasks", true), // duplicate name
        ];
        let err = apply_mcp_peer_set(&catalog, &registry, &bad).unwrap_err();
        assert!(
            err.message.contains("duplicate"),
            "error should mention duplicate: {err}"
        );

        assert_eq!(registry.names(), vec!["tasks".to_string()]);
        assert_eq!(
            catalog
                .routing_descriptors()
                .into_iter()
                .map(|d| d.name)
                .collect::<Vec<_>>(),
            vec!["tasks".to_string()]
        );
    }

    #[test]
    fn reject_empty_command_leaves_prior_set() {
        let catalog = Arc::new(CapabilityCatalog::new());
        let registry = McpRegistry::new().with_health_catalog(catalog.clone());
        apply_mcp_peer_set(&catalog, &registry, &[stdio("tasks", true)]).unwrap();

        let bad = vec![mcp(
            "broken",
            true,
            McpTransport::Stdio {
                command: "  ".into(),
                args: vec![],
            },
        )];
        let err = apply_mcp_peer_set(&catalog, &registry, &bad).unwrap_err();
        assert!(err.message.contains("empty stdio command"), "{err}");

        assert_eq!(registry.names(), vec!["tasks".to_string()]);
        assert!(
            catalog
                .routing_descriptors()
                .iter()
                .any(|d| d.name == "tasks")
        );
    }

    #[test]
    fn runtime_surface_only_lists_applied_peers() {
        let catalog = Arc::new(CapabilityCatalog::new());
        let registry = McpRegistry::new().with_health_catalog(catalog.clone());
        apply_mcp_peer_set(
            &catalog,
            &registry,
            &[stdio("tasks", true), stdio("weather", true)],
        )
        .unwrap();
        apply_mcp_peer_set(&catalog, &registry, &[stdio("tasks", true)]).unwrap();

        // Factory surface + routing catalog must agree after apply (criterion 2).
        assert!(registry.names().contains(&"tasks".to_string()));
        assert!(!registry.names().contains(&"weather".to_string()));
        let routing: HashSet<_> = catalog
            .routing_descriptors()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(routing.contains("tasks"));
        assert!(!routing.contains("weather"));
        // Explicit allow-list of a removed peer finds no connector name on the registry.
        assert!(!registry.names().iter().any(|n| n == "weather"));
    }

    #[test]
    fn empty_boot_then_apply_makes_peers_visible_on_wired_registry() {
        // Composition always holds the registry even when boot peer set is empty.
        let catalog = Arc::new(CapabilityCatalog::new());
        let registry = McpRegistry::new().with_health_catalog(catalog.clone());
        assert!(registry.is_empty());
        assert!(catalog.routing_descriptors().is_empty());

        apply_mcp_peer_set(&catalog, &registry, &[stdio("tasks", true)]).unwrap();
        assert_eq!(registry.names(), vec!["tasks".to_string()]);
        assert_eq!(
            catalog
                .routing_descriptors()
                .into_iter()
                .map(|d| d.name)
                .collect::<Vec<_>>(),
            vec!["tasks".to_string()]
        );
    }

    #[tokio::test]
    async fn live_catalog_gate_refuses_write_without_zone_capability_after_apply() {
        use liberado_common::{Capability, CapabilitySet, ProposalSigner};
        use liberado_executor::{RiskGatedToolRuntime, ToolRuntime};
        use liberado_provider::{ToolDef, ToolInvocation};
        use std::path::PathBuf;

        /// Stub runtime that would execute if the gate let the call through.
        struct WouldExecute;
        #[async_trait::async_trait]
        impl ToolRuntime for WouldExecute {
            fn catalog(&self) -> Vec<ToolDef> {
                vec![ToolDef::new(
                    "vault:write_note",
                    "write",
                    serde_json::json!({"type": "object"}),
                )]
            }
            async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
                Ok("written".into())
            }
        }

        let catalog = Arc::new(CapabilityCatalog::new());
        let registry = McpRegistry::new().with_health_catalog(catalog.clone());

        // Start empty (boot), then apply a path-addressed write MCP — same seam as hot-reload.
        assert!(registry.is_empty());
        let vault_peer = McpConfig {
            name: "vault".into(),
            enabled: true,
            description: "path-addressed vault".into(),
            consequence: liberado_common::Consequence::Reversible,
            transport: McpTransport::Stdio {
                command: "vault-bin".into(),
                args: vec![],
            },
            default_zone: None,
            tools: vec![],
            zone_from_arg: Some("path".into()),
            write_tools: vec!["write_note".into()],
            writes_vault: None,
        };
        apply_mcp_peer_set(&catalog, &registry, &[vault_peer]).unwrap();
        assert!(catalog.get("vault").is_some());

        // Gate uses live catalog (not a boot-time empty snapshot). ExecuteMcp only — no Write(tasks).
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]);
        let gated = RiskGatedToolRuntime::new(
            Arc::new(WouldExecute),
            caps,
            // Stale boot-empty snapshots — live_catalog must override them.
            Vec::new(),
            Vec::new(),
            vec![("tasks".into(), liberado_common::WriteClass::AgentWritable)],
            PathBuf::from("/tmp/liberado-test-proposals"),
            "goal".into(),
            "corr".into(),
            ProposalSigner::random(),
            "default",
        )
        .with_live_catalog(catalog.clone());

        let call = ToolInvocation {
            id: "1".into(),
            name: "vault:write_note".into(),
            arguments: serde_json::json!({"path": "tasks/x.md"}),
        };
        let err = gated.invoke(&call).await.unwrap_err();
        assert!(
            err.contains("not authorized") || err.contains("Write"),
            "live gate must refuse write without Write(tasks): {err}"
        );
        // Peer is on the wired factory surface after empty→apply.
        assert!(registry.names().contains(&"vault".to_string()));
    }

    /// Regression for the reload-ignores-the-config-dir bug: `reload_from_config_dir` used to pass
    /// `None` to `load_config` ("all defaults"), so the real topology on disk was never read and
    /// every hot-reload died with `topology.vault_path is required`. The seam now takes the dir
    /// explicitly, so this asserts a directory with a valid topology actually applies its peers.
    #[test]
    fn reload_from_dir_applies_peers_from_a_real_config_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("topology.toml"),
            r#"
vault_path = "/tmp/liberado-test-vault"

[[mcps]]
name = "tasks-mcp"
description = "create and complete tasks"
consequence = "reversible"
transport = { kind = "stdio", command = "tasks-mcp", args = [] }
writes_vault = false
"#,
        )
        .expect("write topology.toml");

        let controller = LiveMcpController::empty();
        let report = controller
            .reload_from_dir(Some(dir.path()))
            .expect("a valid topology in a real dir must reload");

        assert_eq!(
            report.enabled,
            vec!["tasks-mcp".to_string()],
            "the enabled peer from the on-disk topology must be applied"
        );
        assert!(
            controller
                .registry()
                .names()
                .contains(&"tasks-mcp".to_string())
        );
        assert!(
            controller
                .catalog()
                .routing_descriptors()
                .iter()
                .any(|d| d.name == "tasks-mcp"),
            "the routed catalog must include the reloaded peer"
        );
    }

    /// Pins the `None` = "all defaults" contract that made the bug invisible: with no directory,
    /// the load must fail on the missing `vault_path` rather than silently succeeding on an empty
    /// topology. This is the failure the broken endpoint was reporting on every reload.
    #[test]
    fn reload_from_dir_with_no_directory_reports_missing_vault_path() {
        let controller = LiveMcpController::empty();
        let err = controller
            .reload_from_dir(None)
            .expect_err("no directory is all-defaults and must be rejected");
        assert!(
            err.message.contains("vault_path is required"),
            "the error must name the missing vault path, got: {}",
            err.message
        );
    }

    /// The wrapper itself: `reload_from_config_dir` must read the directory `config_dir()`
    /// resolves (tier-1 `LIBERADO_CONFIG_DIR`), not pass `None`. Exercises the production entry
    /// point end to end so a revert of the fix fails here, not silently at runtime.
    #[test]
    fn reload_from_config_dir_reads_the_librarado_config_dir_env() {
        use std::ffi::OsString;
        use std::sync::{Mutex, OnceLock};

        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _lock = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        struct Guard {
            config: Option<OsString>,
            data: Option<OsString>,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe {
                    match self.config.take() {
                        Some(v) => std::env::set_var("LIBERADO_CONFIG_DIR", v),
                        None => std::env::remove_var("LIBERADO_CONFIG_DIR"),
                    }
                    match self.data.take() {
                        Some(v) => std::env::set_var("LIBERADO_DATA_DIR", v),
                        None => std::env::remove_var("LIBERADO_DATA_DIR"),
                    }
                }
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let data = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("topology.toml"),
            r#"
vault_path = "/tmp/liberado-test-vault"

[[mcps]]
name = "env-dir-mcp"
description = "reachable only through config_dir resolution"
consequence = "read_only"
transport = { kind = "stdio", command = "env-dir-mcp", args = [] }
"#,
        )
        .expect("write topology.toml");

        let _guard = Guard {
            config: std::env::var_os("LIBERADO_CONFIG_DIR"),
            data: std::env::var_os("LIBERADO_DATA_DIR"),
        };
        unsafe {
            std::env::set_var("LIBERADO_CONFIG_DIR", dir.path());
            std::env::set_var("LIBERADO_DATA_DIR", data.path());
        }

        let controller = LiveMcpController::empty();
        let report = controller
            .reload_from_config_dir()
            .expect("the wrapper must resolve and read the config dir");

        assert_eq!(
            report.enabled,
            vec!["env-dir-mcp".to_string()],
            "the peer from the config-dir topology must be applied"
        );
    }
}

#[cfg(test)]
#[path = "mcp_apply_survivor_tests.rs"]
mod survivor_tests;
