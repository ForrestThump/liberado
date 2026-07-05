//! # Merged-config validation
//!
//! Cross-cutting checks that span multiple sections of a [`Config`] and therefore can
//! only be verified after all sources have been loaded and merged.
//!
//! 1. Every zone named in a grant capability must be declared in `policy.zones`.
//! 2. Every `ExecuteMcp(name)` in a grant must name an MCP present in `topology.mcps`.
//! 3. Every `policy.secret_refs` entry must be set as an environment variable.
//! 4. Every `topology.hooks[].secret_ref` must be set as an environment variable, same as #3.
//!
//! These checks are the "merged-config" slice of Decision 14's fail-fast contract.
//! They live here — in the loader crate — alongside the merging machinery, so that
//! every code path that assembles a [`Config`] can run them without depending on the
//! bootstrap crate.

use liberado_common::Zone;
use liberado_common::capability::Capability;

use crate::model::Config;
use crate::source::ConfigLoadError;

/// Validate cross-cutting invariants that span [`Config`] sections.
///
/// Returns the first violation found. Each error message names the offending entry so
/// the user can fix it without a debugger.
pub fn validate_merged_config(config: &Config) -> Result<(), ConfigLoadError> {
    let invalid = |msg: String| ConfigLoadError::Validation(msg);

    for grant in &config.policy.grants {
        for cap in &grant.capabilities {
            match cap {
                Capability::Read(zone)
                | Capability::Write(zone)
                | Capability::ReadSummary(zone) => {
                    let name = zone_name(zone);
                    if !config.policy.zones.iter().any(|z| z.zone == name) {
                        return Err(invalid(format!(
                            "grant references undeclared zone '{name}'"
                        )));
                    }
                }
                Capability::ExecuteMcp(mcp) => {
                    if !config.topology.mcps.iter().any(|c| &c.name == mcp) {
                        return Err(invalid(format!(
                            "grant references unknown MCP '{mcp}' (not in topology.mcps)"
                        )));
                    }
                }
            }
        }
    }

    for secret in &config.policy.secret_refs {
        if std::env::var_os(secret).is_none() {
            return Err(invalid(format!(
                "secret_ref '{secret}' has no corresponding environment variable"
            )));
        }
    }

    for hook in &config.topology.hooks {
        if std::env::var_os(&hook.secret_ref).is_none() {
            return Err(invalid(format!(
                "topology.hooks['{}'].secret_ref '{}' has no corresponding environment variable",
                hook.name, hook.secret_ref
            )));
        }
    }

    Ok(())
}

/// The bare name a zone capability references, regardless of vault/named kind — both
/// are matched against `policy.zones` by name (zones are declared by name, not by kind).
fn zone_name(zone: &Zone) -> &str {
    match zone {
        Zone::Vault(name) | Zone::Named(name) => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Grant, McpConfig, McpTransport, Policy, Topology, ZonePolicy};
    use liberado_common::capability::Consequence;

    /// Helper: a minimal valid topology with one MCP.
    fn topology_with_mcp(name: &str) -> Topology {
        Topology {
            vault_path: "/home/test/vault".into(),
            mcps: vec![McpConfig {
                name: name.into(),
                enabled: true,
                description: "test mcp".into(),
                consequence: Consequence::Reversible,
                transport: McpTransport::Stdio {
                    command: "test".into(),
                    args: vec![],
                },
                default_zone: None,
                tools: Vec::new(),
            }],
            ..Topology::default()
        }
    }

    fn config(topology: Topology, policy: Policy) -> Config {
        Config {
            topology,
            policy,
            ..Config::default()
        }
    }

    #[test]
    fn valid_config_passes() {
        let cfg = config(
            topology_with_mcp("memory-mcp"),
            Policy {
                zones: vec![ZonePolicy {
                    zone: "tasks".into(),
                    write_class: liberado_common::WriteClass::AgentWritable,
                }],
                grants: vec![Grant {
                    component: "agent".into(),
                    capabilities: vec![
                        Capability::Read(Zone::vault("tasks")),
                        Capability::Write(Zone::vault("tasks")),
                        Capability::ExecuteMcp("memory-mcp".into()),
                    ],
                }],
                secret_refs: vec![],
            },
        );
        assert!(validate_merged_config(&cfg).is_ok());
    }

    #[test]
    fn rejects_undeclared_zone() {
        let cfg = config(
            topology_with_mcp("mcp"),
            Policy {
                zones: vec![ZonePolicy {
                    zone: "tasks".into(),
                    write_class: liberado_common::WriteClass::AgentWritable,
                }],
                grants: vec![Grant {
                    component: "agent".into(),
                    capabilities: vec![Capability::Write(Zone::vault("decisions"))],
                }],
                secret_refs: vec![],
            },
        );
        let err = validate_merged_config(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("undeclared zone"), "got: {msg}");
        assert!(msg.contains("decisions"), "should name the zone: {msg}");
    }

    #[test]
    fn rejects_unknown_mcp() {
        let cfg = config(
            topology_with_mcp("real-mcp"),
            Policy {
                zones: vec![],
                grants: vec![Grant {
                    component: "agent".into(),
                    capabilities: vec![Capability::ExecuteMcp("ghost-mcp".into())],
                }],
                secret_refs: vec![],
            },
        );
        let err = validate_merged_config(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown MCP"), "got: {msg}");
        assert!(msg.contains("ghost-mcp"), "should name the MCP: {msg}");
    }

    #[test]
    fn rejects_secret_ref_with_no_env_var() {
        let cfg = config(
            topology_with_mcp("mcp"),
            Policy {
                zones: vec![],
                grants: vec![],
                secret_refs: vec!["LIBERADO_TEST_DEFINITELY_UNSET_SECRET_XYZZY".into()],
            },
        );
        let err = validate_merged_config(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("secret_ref"), "got: {msg}");
        assert!(
            msg.contains("LIBERADO_TEST_DEFINITELY_UNSET_SECRET_XYZZY"),
            "should name the secret: {msg}"
        );
    }

    #[test]
    fn rejects_hook_secret_ref_with_no_env_var() {
        let mut topology = topology_with_mcp("mcp");
        topology.hooks = vec![crate::model::HookConfig {
            name: "nightly-backup".into(),
            enabled: true,
            secret_ref: "LIBERADO_TEST_DEFINITELY_UNSET_HOOK_SECRET_XYZZY".into(),
            goal: "do something".into(),
            pool: None,
        }];
        let cfg = config(
            topology,
            Policy {
                zones: vec![],
                grants: vec![],
                secret_refs: vec![],
            },
        );
        let err = validate_merged_config(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("secret_ref"), "got: {msg}");
        assert!(
            msg.contains("LIBERADO_TEST_DEFINITELY_UNSET_HOOK_SECRET_XYZZY"),
            "should name the secret: {msg}"
        );
    }

    #[test]
    fn empty_policy_passes() {
        let cfg = config(
            topology_with_mcp("mcp"),
            Policy {
                zones: vec![],
                grants: vec![],
                secret_refs: vec![],
            },
        );
        assert!(validate_merged_config(&cfg).is_ok());
    }
}
