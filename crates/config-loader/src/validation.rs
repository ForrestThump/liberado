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
fn validation_error(msg: String) -> ConfigLoadError {
    ConfigLoadError::Validation(msg)
}

/// Validate that a zone capability names a zone declared in `policy.zones`.
fn validate_zone_capability(config: &Config, zone: &Zone) -> Result<(), ConfigLoadError> {
    let name = zone_name(zone);
    if !config.policy.zones.iter().any(|z| z.zone == name) {
        return Err(validation_error(format!(
            "grant references undeclared zone '{name}'"
        )));
    }
    Ok(())
}

/// Validate that an `ExecuteMcp` capability names an MCP present in `topology.mcps`.
fn validate_execute_mcp(config: &Config, mcp: &String) -> Result<(), ConfigLoadError> {
    if !config.topology.mcps.iter().any(|c| &c.name == mcp) {
        return Err(validation_error(format!(
            "grant references unknown MCP '{mcp}' (not in topology.mcps)"
        )));
    }
    Ok(())
}

/// Validate an `ExecuteTool("<mcp>:<tool>")` capability.
///
/// Only the server half is checkable: `topology.mcps` is wiring, and an MCP's tool names are not
/// known until it connects. So validate the prefix and, above all, insist there *is* one.
///
/// A missing `:` is the trap worth catching here. `ExecuteTool("read_note")` parses fine and then
/// means "the MCP named read_note" everywhere downstream — a grant that authorizes nothing,
/// silently, in a file whose whole job is to authorize things.
fn validate_execute_tool(config: &Config, qualified: &String) -> Result<(), ConfigLoadError> {
    let Some((mcp, tool)) = qualified.split_once(':') else {
        return Err(validation_error(format!(
            "grant has ExecuteTool '{qualified}' with no ':' — it must be '<mcp>:<tool>' (e.g. \
             'turbovault:read_note'), or it silently grants nothing. Use ExecuteMcp to grant a \
             whole server."
        )));
    };
    if tool.is_empty() {
        return Err(validation_error(format!(
            "grant has ExecuteTool '{qualified}' with an empty tool name — use ExecuteMcp = \
             '{mcp}' to grant the whole server"
        )));
    }
    if !config.topology.mcps.iter().any(|c| c.name == mcp) {
        return Err(validation_error(format!(
            "grant references unknown MCP '{mcp}' in ExecuteTool '{qualified}' (not in \
             topology.mcps)"
        )));
    }
    Ok(())
}

/// Validate that every capability in every grant resolves: zones declared, MCPs present, tool
/// names well-formed.
fn validate_grants(config: &Config) -> Result<(), ConfigLoadError> {
    for grant in &config.policy.grants {
        for cap in &grant.capabilities {
            match cap {
                Capability::Read(zone)
                | Capability::Write(zone)
                | Capability::ReadSummary(zone) => validate_zone_capability(config, zone)?,
                Capability::ExecuteMcp(mcp) => validate_execute_mcp(config, mcp)?,
                Capability::ExecuteTool(qualified) => validate_execute_tool(config, qualified)?,
                // Self-contained: names no zone and no MCP, so there is nothing to resolve.
                Capability::AskHuman => {}
            }
        }
    }
    Ok(())
}

/// 5. An MCP that can change something must say WHAT it changes (F1, 2026-07-14).
///
/// Zone declaration used to be opt-in, and the consequences were not theoretical: with no MCP in
/// `topology.toml` declaring a zone, `resolve_zone` returned `None` for every tool, so the
/// zone-write-class guard never fired *once* — and the `Capability::Write` check that now sits
/// beside it would have been equally inert. A dispatch session granted `Read` and no `Write`
/// wrote a vault note, live.
///
/// So zone joins `description`, `consequence` and `transport` in the rule this config already
/// states: **declaring an MCP means rating it, wiring it, and saying what it touches.** Failing
/// here — loudly, at boot, naming the MCP — rather than at the tool call is deliberate: a
/// refusal mid-conversation is discovered three turns into something you cared about, with an
/// error that does not explain itself. `read_only` MCPs are exempt: they write nothing, so
/// there is no zone to name.
fn validate_mcp_zones(config: &Config) -> Result<(), ConfigLoadError> {
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
            return Err(validation_error(format!(
                "MCP '{}' sets `zone_from_arg` but lists no `write_tools`. A path argument alone \
                 cannot tell a read from a write (`read_note` and `write_note` both have one), so \
                 without this list either every read would demand a Write capability, or no write \
                 would be checked at all. Name the tools that write.",
                mcp.name
            )));
        }
        if !fixed_zone && !path_addressed {
            return Err(validation_error(format!(
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
    Ok(())
}

/// 6. A declared report sink must actually be able to write (Decision 14).
///
/// The whole point of `Delivery::Vault` is that no model is in the loop: the orchestrator makes
/// one deterministic tool call and reports where the note went. That leaves nothing to notice a
/// sink pointing at a missing MCP, a disabled one, or — worst — a *read* tool, which would
/// return happily and write nothing while the receipt claimed a path. The report would simply
/// not exist, and the human would find out by opening the file. So the sink is checked here,
/// where the failure is a daemon that refuses to start and says why.
fn validate_report_sink(config: &Config) -> Result<(), ConfigLoadError> {
    if let Some(sink) = &config.topology.report_sink {
        let Some(mcp) = config.topology.mcps.iter().find(|m| m.name == sink.mcp) else {
            return Err(validation_error(format!(
                "topology.report_sink.mcp '{}' is not in topology.mcps",
                sink.mcp
            )));
        };
        if !mcp.enabled {
            return Err(validation_error(format!(
                "topology.report_sink.mcp '{}' is disabled — a report delivered to it would be \
                 silently lost. Enable it or remove the sink.",
                sink.mcp
            )));
        }
        if mcp.consequence == liberado_common::Consequence::ReadOnly {
            return Err(validation_error(format!(
                "topology.report_sink.mcp '{}' is rated read_only, so it cannot write the report. \
                 Point the sink at the vault MCP.",
                sink.mcp
            )));
        }
        // Only checkable for a path-addressed MCP — a fixed-zone MCP doesn't enumerate its writers.
        if !mcp.write_tools.is_empty() && !mcp.write_tools.contains(&sink.tool) {
            return Err(validation_error(format!(
                "topology.report_sink.tool '{}' is not among MCP '{}'s `write_tools` ({}). A \
                 sink that is really a read would return success and write nothing.",
                sink.tool,
                sink.mcp,
                mcp.write_tools.join(", ")
            )));
        }
        if sink.path_arg.trim().is_empty() || sink.content_arg.trim().is_empty() {
            return Err(validation_error(
                "topology.report_sink.path_arg and .content_arg must be non-empty argument names"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// 3. Every `policy.secret_refs` entry must be set as an environment variable.
fn validate_policy_secrets(config: &Config) -> Result<(), ConfigLoadError> {
    for secret in &config.policy.secret_refs {
        if std::env::var_os(secret).is_none() {
            return Err(validation_error(format!(
                "secret_ref '{secret}' has no corresponding environment variable"
            )));
        }
    }
    Ok(())
}

/// 4. Every `topology.hooks[].secret_ref` must be set as an environment variable, same as #3.
fn validate_hook_secrets(config: &Config) -> Result<(), ConfigLoadError> {
    for hook in &config.topology.hooks {
        if std::env::var_os(&hook.secret_ref).is_none() {
            return Err(validation_error(format!(
                "topology.hooks['{}'].secret_ref '{}' has no corresponding environment variable",
                hook.name, hook.secret_ref
            )));
        }
    }
    Ok(())
}

/// 7. Every `[[risk_waivers]]` entry must reference an MCP that exists, and every zone named in
///    `match_zones` must be a declared `[[zones]]`.
///
/// The risk waiver feature exists to suppress the magnitude heuristic for reads — a config that
/// names a non-existent MCP, or a zone no `[[zones]]` block declares, would silently never match
/// at all. Catching it at boot is the same fail-fast discipline the rest of the loader uses: a
/// refused config with a clear message is better than a config that runs and never does the thing
/// the operator wrote it for.
fn validate_risk_waivers(config: &Config) -> Result<(), ConfigLoadError> {
    for waiver in &config.policy.risk_waivers {
        if !config.topology.mcps.iter().any(|m| m.name == waiver.mcp) {
            return Err(validation_error(format!(
                "risk_waiver references unknown MCP '{}' (not in topology.mcps)",
                waiver.mcp
            )));
        }
        if let Some(zones) = &waiver.match_zones {
            for zone in zones {
                if !config.policy.zones.iter().any(|z| &z.zone == zone) {
                    return Err(validation_error(format!(
                        "risk_waiver for MCP '{}' references undeclared zone '{zone}'",
                        waiver.mcp
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Validate cross-cutting invariants that span [`Config`] sections.
///
/// Returns the first violation found. Each error message names the offending entry so the user
/// can fix it without a debugger.
pub fn validate_merged_config(config: &Config) -> Result<(), ConfigLoadError> {
    validate_grants(config)?;
    validate_mcp_zones(config)?;
    validate_report_sink(config)?;
    validate_policy_secrets(config)?;
    validate_hook_secrets(config)?;
    validate_risk_waivers(config)?;
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
    use crate::model::{
        Grant, McpConfig, McpTransport, Policy, ReportSinkConfig, Topology, ZonePolicy,
    };
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
                risk_waivers: vec![],
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
                risk_waivers: vec![],
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
                risk_waivers: vec![],
            },
        );
        let err = validate_merged_config(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown MCP"), "got: {msg}");
        assert!(msg.contains("ghost-mcp"), "should name the MCP: {msg}");
    }

    /// `ExecuteTool` grants must be qualified: a missing `:` would silently authorize nothing (the
    /// whole string becomes an MCP name), an empty tool name is equally meaningless, and a prefix
    /// naming an undeclared MCP is dead weight. All three are refused at load time.
    #[test]
    fn rejects_malformed_execute_tool_capabilities() {
        let cfg = |cap: Capability| {
            config(
                topology_with_mcp("memory-mcp"),
                Policy {
                    zones: vec![],
                    grants: vec![Grant {
                        component: "agent".into(),
                        capabilities: vec![cap],
                    }],
                    secret_refs: vec![],
                    risk_waivers: vec![],
                },
            )
        };

        let bare = cfg(Capability::ExecuteTool("read_note".into()));
        let msg = validate_merged_config(&bare).unwrap_err().to_string();
        assert!(
            msg.contains("no ':'"),
            "bare tool name must be refused: {msg}"
        );

        let empty = cfg(Capability::ExecuteTool("memory-mcp:".into()));
        let msg = validate_merged_config(&empty).unwrap_err().to_string();
        assert!(msg.contains("empty tool name"), "got: {msg}");

        let ghost = cfg(Capability::ExecuteTool("ghost-mcp:read_note".into()));
        let msg = validate_merged_config(&ghost).unwrap_err().to_string();
        assert!(msg.contains("unknown MCP"), "got: {msg}");
        assert!(msg.contains("ghost-mcp"), "should name the MCP: {msg}");

        let ok = cfg(Capability::ExecuteTool("memory-mcp:read_note".into()));
        assert!(
            validate_merged_config(&ok).is_ok(),
            "a well-formed, resolvable tool grant must pass"
        );
    }

    #[test]
    fn rejects_secret_ref_with_no_env_var() {
        let cfg = config(
            topology_with_mcp("mcp"),
            Policy {
                zones: vec![],
                grants: vec![],
                secret_refs: vec!["LIBERADO_TEST_DEFINITELY_UNSET_SECRET_XYZZY".into()],
                risk_waivers: vec![],
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
            profile: None,
        }];
        let cfg = config(
            topology,
            Policy {
                zones: vec![],
                grants: vec![],
                secret_refs: vec![],
                risk_waivers: vec![],
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
                risk_waivers: vec![],
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

    // --- Report sink (#6) ---------------------------------------------------------------------
    //
    // Vault delivery runs with no model in the loop, so nothing downstream can notice a sink that
    // points somewhere useless. A misdeclared sink would mean the report silently does not exist
    // while the receipt names a path — so it is caught here, at boot.

    /// A vault-shaped MCP: path-addressed, with a real writer among its tools.
    fn topology_with_vault() -> Topology {
        let mut topology = topology_with_mcp("turbovault");
        topology.mcps[0].writes_vault = None;
        topology.mcps[0].zone_from_arg = Some("path".into());
        topology.mcps[0].write_tools = vec!["write_note".into(), "delete_note".into()];
        topology
    }

    fn sink(mcp: &str, tool: &str) -> ReportSinkConfig {
        ReportSinkConfig {
            mcp: mcp.into(),
            tool: tool.into(),
            path_arg: "path".into(),
            content_arg: "content".into(),
        }
    }

    #[test]
    fn a_well_formed_report_sink_validates() {
        let mut topology = topology_with_vault();
        topology.report_sink = Some(sink("turbovault", "write_note"));
        assert!(validate_merged_config(&config(topology, Policy::default())).is_ok());
    }

    #[test]
    fn a_report_sink_naming_an_unknown_mcp_is_refused() {
        let mut topology = topology_with_vault();
        topology.report_sink = Some(sink("nope", "write_note"));
        let err = validate_merged_config(&config(topology, Policy::default()))
            .expect_err("unknown sink MCP must fail at boot");
        assert!(format!("{err}").contains("not in topology.mcps"));
    }

    /// The nastiest case: the sink resolves and the tool exists, but it is a *read*. It would
    /// return success and write nothing, and the receipt would name a file that was never created.
    #[test]
    fn a_report_sink_pointing_at_a_read_tool_is_refused() {
        let mut topology = topology_with_vault();
        topology.report_sink = Some(sink("turbovault", "read_note"));
        let err = validate_merged_config(&config(topology, Policy::default()))
            .expect_err("a sink that is really a read must fail at boot");
        assert!(format!("{err}").contains("write_tools"));
    }

    #[test]
    fn a_report_sink_on_a_disabled_or_read_only_mcp_is_refused() {
        let mut topology = topology_with_vault();
        topology.report_sink = Some(sink("turbovault", "write_note"));
        topology.mcps[0].enabled = false;
        assert!(validate_merged_config(&config(topology.clone(), Policy::default())).is_err());

        topology.mcps[0].enabled = true;
        topology.mcps[0].consequence = Consequence::ReadOnly;
        let err = validate_merged_config(&config(topology, Policy::default()))
            .expect_err("a read_only MCP cannot be a write sink");
        assert!(format!("{err}").contains("read_only"));
    }

    /// No sink declared is not an error — it just means vault delivery is unavailable and every
    /// report is summarized, exactly as before this existed.
    #[test]
    fn no_report_sink_is_a_valid_configuration() {
        let topology = topology_with_vault();
        assert!(topology.report_sink.is_none());
        assert!(validate_merged_config(&config(topology, Policy::default())).is_ok());
    }

    /// An MCP with default_zone set but no per-tool zones — `||` on line 106 requires only one of
    /// the two to be present; `&&` would demand both.
    #[test]
    fn default_zone_alone_satisfies_zone_requirement() {
        let mut topology = topology_with_mcp("turbovault");
        topology.mcps[0].writes_vault = None;
        topology.mcps[0].default_zone = Some("tasks".into());
        assert!(validate_merged_config(&config(topology, Policy::default())).is_ok());
    }

    /// A report sink with an empty path_arg should fail even when content_arg is valid. The `||`
    /// on line 173 catches this; `&&` would let it through.
    #[test]
    fn report_sink_empty_path_arg_is_refused() {
        let mut topology = topology_with_vault();
        topology.report_sink = Some(ReportSinkConfig {
            path_arg: "".into(),
            content_arg: "content".into(),
            ..sink("turbovault", "write_note")
        });
        assert!(validate_merged_config(&config(topology, Policy::default())).is_err());
    }

    #[test]
    fn report_sink_empty_content_arg_is_refused() {
        let mut topology = topology_with_vault();
        topology.report_sink = Some(ReportSinkConfig {
            path_arg: "path".into(),
            content_arg: "".into(),
            ..sink("turbovault", "write_note")
        });
        assert!(validate_merged_config(&config(topology, Policy::default())).is_err());
    }

    // --- Risk waivers (#7) ---------------------------------------------------------------------
    //
    // The risk-waiver feature exists to suppress the magnitude heuristic for reads. A waiver that
    // names a non-existent MCP, or a zone no `[[zones]]` block declares, would silently never
    // match — catching it at boot is the same fail-fast discipline the rest of the loader uses.

    fn waiver(
        mcp: &str,
        zones: Option<Vec<&str>>,
        tools: Option<Vec<&str>>,
    ) -> liberado_common::RiskWaiver {
        liberado_common::RiskWaiver {
            mcp: mcp.into(),
            match_tools: tools.map(|t| t.into_iter().map(String::from).collect()),
            match_zones: zones.map(|z| z.into_iter().map(String::from).collect()),
            guard: liberado_common::Guard::Magnitude,
        }
    }

    #[test]
    fn risk_waiver_with_no_zones_or_tools_passes() {
        let cfg = config(
            topology_with_mcp("weather-mcp"),
            Policy {
                zones: vec![],
                grants: vec![],
                secret_refs: vec![],
                risk_waivers: vec![waiver("weather-mcp", None, None)],
            },
        );
        assert!(validate_merged_config(&cfg).is_ok());
    }

    #[test]
    fn risk_waiver_referencing_unknown_mcp_is_refused() {
        let cfg = config(
            topology_with_mcp("weather-mcp"),
            Policy {
                zones: vec![],
                grants: vec![],
                secret_refs: vec![],
                risk_waivers: vec![waiver("ghost-mcp", None, None)],
            },
        );
        let err = validate_merged_config(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown MCP"), "got: {msg}");
        assert!(msg.contains("ghost-mcp"), "should name the offender: {msg}");
    }

    #[test]
    fn risk_waiver_referencing_undeclared_zone_is_refused() {
        let cfg = config(
            topology_with_mcp("turbovault"),
            Policy {
                zones: vec![ZonePolicy {
                    zone: "tasks".into(),
                    write_class: liberado_common::WriteClass::AgentWritable,
                }],
                grants: vec![],
                secret_refs: vec![],
                risk_waivers: vec![waiver("turbovault", Some(vec!["finance"]), None)],
            },
        );
        let err = validate_merged_config(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("undeclared zone"), "got: {msg}");
        assert!(msg.contains("finance"), "should name the zone: {msg}");
    }

    #[test]
    fn risk_waiver_with_declared_zones_passes() {
        let cfg = config(
            topology_with_mcp("turbovault"),
            Policy {
                zones: vec![
                    ZonePolicy {
                        zone: "Tasks".into(),
                        write_class: liberado_common::WriteClass::AgentWritable,
                    },
                    ZonePolicy {
                        zone: "Work".into(),
                        write_class: liberado_common::WriteClass::AgentWritable,
                    },
                ],
                grants: vec![],
                secret_refs: vec![],
                risk_waivers: vec![waiver(
                    "turbovault",
                    Some(vec!["Tasks", "Work"]),
                    Some(vec!["read_note"]),
                )],
            },
        );
        assert!(
            validate_merged_config(&cfg).is_ok(),
            "a waiver with declared zones and a real MCP must pass"
        );
    }
}
