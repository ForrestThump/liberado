//! # Merged-config validation
//!
//! Cross-cutting checks that span multiple sections of a [`Config`] and therefore can
//! only be verified after all sources have been loaded and merged.
//!
//! 1. Every zone named in a grant capability must be declared in `policy.zones`.
//! 2. Every `ExecuteMcp(name)` in a grant must name an MCP present in `topology.mcps`.
//! 3. Every `policy.secret_refs` entry must be set as an environment variable.
//! 4. Every `topology.hooks[].secret_ref` must be set as an environment variable, same as #3.
//! 5. Every non-`read_only` MCP must declare the zone its writes land in (F1).
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
                // Self-contained: names no zone and no MCP, so there is nothing to resolve.
                Capability::AskHuman => {}
            }
        }
    }

    // 5. An MCP that can change something must say WHAT it changes (F1, 2026-07-14).
    //
    // Zone declaration used to be opt-in, and the consequences were not theoretical: with no MCP in
    // `topology.toml` declaring a zone, `resolve_zone` returned `None` for every tool, so the
    // zone-write-class guard never fired *once* — and the `Capability::Write` check that now sits
    // beside it would have been equally inert. A dispatch session granted `Read` and no `Write`
    // wrote a vault note, live.
    //
    // So zone joins `description`, `consequence` and `transport` in the rule this config already
    // states: **declaring an MCP means rating it, wiring it, and saying what it touches.** Failing
    // here — loudly, at boot, naming the MCP — rather than at the tool call is deliberate: a
    // refusal mid-conversation is discovered three turns into something you cared about, with an
    // error that does not explain itself. `read_only` MCPs are exempt: they write nothing, so
    // there is no zone to name.
    for mcp in config.topology.mcps.iter().filter(|m| m.enabled) {
        if mcp.consequence == liberado_common::Consequence::ReadOnly {
            continue;
        }
        // An explicit "I write no vault zones" satisfies the rule. Silence does not.
        if mcp.writes_vault == Some(false) {
            continue;
        }
        let fixed_zone = mcp.default_zone.is_some() || mcp.tools.iter().any(|t| t.zone.is_some());
        let path_addressed = mcp.zone_from_arg.is_some();
        if path_addressed && mcp.write_tools.is_empty() {
            return Err(invalid(format!(
                "MCP '{}' sets `zone_from_arg` but lists no `write_tools`. A path argument alone \
                 cannot tell a read from a write (`read_note` and `write_note` both have one), so \
                 without this list either every read would demand a Write capability, or no write \
                 would be checked at all. Name the tools that write.",
                mcp.name
            )));
        }
        if !fixed_zone && !path_addressed {
            return Err(invalid(format!(
                "MCP '{}' is '{:?}' (not read_only) but declares no write zone, so the capability \
                 guard cannot tell what its writes touch — meaning ANY grant holding \
                 ExecuteMcp(\"{}\") could write anything this MCP can reach, whatever its Write \
                 capabilities say. Declare one of:\n  \
                 • `default_zone = \"<zone>\"` (+ optional per-tool `[[mcps.tools]]` overrides) — a \
                 fixed-zone MCP;\n  \
                 • `zone_from_arg = \"path\"` + `write_tools = [...]` — a path-addressed MCP whose \
                 zone depends on the call;\n  \
                 • `writes_vault = false` — it has effects, but none of them are vault writes;\n  \
                 • or rate it `read_only` if it changes nothing at all.",
                mcp.name, mcp.consequence, mcp.name
            )));
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
                zone_from_arg: None,
                write_tools: Vec::new(),
                // These tests are about grants/zones/secrets, not about F1's zone rule. Declaring
                // "no vault writes" keeps them focused — and the fact that this line is *required*
                // for them to pass at all is the rule doing its job.
                writes_vault: Some(false),
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

    /// F1: a writing MCP that says nothing about zones must not boot.
    #[test]
    fn a_writing_mcp_that_declares_no_zone_refuses_to_boot() {
        let mut topology = topology_with_mcp("turbovault");
        topology.mcps[0].writes_vault = None; // the old default: silence

        let err = validate_merged_config(&config(topology, Policy::default()))
            .expect_err("silence about zones is what F1 was — it must not be accepted");
        let msg = err.to_string();
        assert!(msg.contains("turbovault"), "must name the offender: {msg}");
        assert!(
            msg.contains("write zone") || msg.contains("writes_vault"),
            "and must say how to fix it: {msg}"
        );
    }

    #[test]
    fn a_read_only_mcp_needs_no_zone() {
        let mut topology = topology_with_mcp("deepwiki");
        topology.mcps[0].consequence = Consequence::ReadOnly;
        topology.mcps[0].writes_vault = None;

        assert!(
            validate_merged_config(&config(topology, Policy::default())).is_ok(),
            "a read-only MCP writes nothing, so it has no zone to declare"
        );
    }

    #[test]
    fn a_path_addressed_mcp_must_say_which_tools_write() {
        // `zone_from_arg` alone cannot tell `read_note` from `write_note` — both carry a path.
        let mut topology = topology_with_mcp("turbovault");
        topology.mcps[0].writes_vault = None;
        topology.mcps[0].zone_from_arg = Some("path".into());
        topology.mcps[0].write_tools = Vec::new();

        let err = validate_merged_config(&config(topology, Policy::default()))
            .expect_err("zone_from_arg without write_tools cannot distinguish reads from writes");
        assert!(err.to_string().contains("write_tools"), "{err}");
    }

    #[test]
    fn declaring_no_vault_writes_is_accepted_because_it_is_a_statement() {
        // The escape hatch exists so the opt-out is explicit. Silence is a bug; saying so is fine.
        let mut topology = topology_with_mcp("liberado-pdf-mcp");
        topology.mcps[0].writes_vault = Some(false);
        assert!(validate_merged_config(&config(topology, Policy::default())).is_ok());
    }
}
