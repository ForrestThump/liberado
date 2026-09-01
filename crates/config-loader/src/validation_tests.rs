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
