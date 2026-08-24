//! Unit tests for `write_explanation`, the pure half of `explain_write`.
//!
//! The explainer mirrors the live guard pipeline (mcp_declared → mcp_grant → write_target →
//! write_capability → zone_write_class → consequence) and prints every verdict rather than
//! stopping at the first `no`. These tests pin each guard's verdict and the accumulated fix
//! lines, from config alone — no stdout capture, no files on disk.

use super::*;

use liberado_common::{Capability, Consequence, WriteClass, Zone};
use liberado_config::{Grant, McpConfig, McpTransport, ToolImpact, ZonePolicy};

fn vault_mcp(consequence: Consequence) -> McpConfig {
    McpConfig {
        name: "turbovault".into(),
        enabled: true,
        description: "vault tools".into(),
        consequence,
        transport: McpTransport::Managed,
        default_zone: None,
        tools: vec![ToolImpact {
            name: "read_note".into(),
            zone: None,
        }],
        zone_from_arg: Some("path".into()),
        write_tools: vec!["write_note".into()],
        writes_vault: None,
    }
}

fn config(
    mcps: Vec<McpConfig>,
    grants: Vec<Grant>,
    zones: Vec<(String, WriteClass)>,
) -> liberado_bootstrap::Config {
    let mut config = liberado_bootstrap::Config::default();
    config.topology.mcps = mcps;
    config.policy.grants = grants;
    config.policy.zones = zones
        .into_iter()
        .map(|(zone, write_class)| ZonePolicy { zone, write_class })
        .collect();
    config
}

fn grant(component: &str, capabilities: Vec<Capability>) -> Grant {
    Grant {
        component: component.into(),
        capabilities,
    }
}

fn agent_writable(zone: &str) -> (String, WriteClass) {
    (zone.into(), WriteClass::AgentWritable)
}

fn explain(config: &liberado_bootstrap::Config, tool: &str, path: &str) -> WriteExplanation {
    write_explanation(config, "main-agent", tool, path)
}

#[test]
fn an_unenabled_mcp_blocks_before_any_other_guard_runs() {
    let config = config(vec![], vec![], vec![]);
    let e = explain(&config, "ghost:tool", "some/path");
    assert_eq!(e.guard_lines.len(), 1);
    assert!(
        e.guard_lines[0].contains("[BLOCK] mcp_declared"),
        "expected the undeclared-MCP line, got {:?}",
        e.guard_lines[0]
    );
    assert_eq!(
        e.verdict,
        "verdict: BLOCKED — the MCP does not exist in this config."
    );
    assert!(e.fixes.is_empty(), "an absent MCP has no fix to suggest");
}

#[test]
fn a_missing_grant_names_the_policy_line_to_add() {
    let config = config(vec![vault_mcp(Consequence::Reversible)], vec![], vec![]);
    let e = explain(&config, "turbovault:read_note", "tasks/x.md");
    assert!(
        e.guard_lines[0].contains("[BLOCK] mcp_grant"),
        "expected a blocked grant line, got {:?}",
        e.guard_lines[0]
    );
    // Without an ExecuteMcp grant the needed-capability text names both spellings.
    assert!(
        e.guard_lines[0]
            .contains("ExecuteMcp(\"turbovault\") or ExecuteTool(\"turbovault:read_note\")"),
        "expected both capability spellings, got {:?}",
        e.guard_lines[0]
    );
    assert!(e.fixes.len() == 1 && e.fixes[0].contains("policy.toml"));
    // A read is still a read: no write guards ran after the grant check.
    assert_eq!(e.guard_lines.len(), 2);
    assert_eq!(e.verdict, "verdict: BLOCKED");
}

#[test]
fn a_server_wide_grant_reads_the_tool_spelling_of_the_needed_capability() {
    let config = config(
        vec![vault_mcp(Consequence::Reversible)],
        vec![grant(
            "main-agent",
            vec![Capability::ExecuteMcp("turbovault".into())],
        )],
        vec![],
    );
    let e = explain(&config, "turbovault:read_note", "tasks/x.md");
    assert!(
        e.guard_lines[0].contains("ExecuteTool(\"turbovault:read_note\")"),
        "a server-wide grant needs only the tool spelling echoed, got {:?}",
        e.guard_lines[0]
    );
    assert_eq!(e.verdict, "verdict: ALLOWED");
    assert!(e.fixes.is_empty());
}

#[test]
fn a_read_on_a_granted_mcp_is_allowed_without_write_guards() {
    let config = config(
        vec![vault_mcp(Consequence::Irreversible)],
        vec![grant(
            "main-agent",
            vec![Capability::ExecuteTool("turbovault:read_note".into())],
        )],
        vec![],
    );
    let e = explain(&config, "turbovault:read_note", "tasks/x.md");
    assert!(
        e.guard_lines[1].contains("is a read on this MCP"),
        "expected the not-a-write line, got {:?}",
        e.guard_lines[1]
    );
    assert_eq!(e.verdict, "verdict: ALLOWED");
}

#[test]
fn a_read_still_reports_blocked_when_the_grant_is_missing() {
    // The not-a-write short-circuit reports a bare BLOCKED (no guard count) even though the
    // grant already failed — the write guards never ran, so there is nothing to count.
    let config = config(vec![vault_mcp(Consequence::Reversible)], vec![], vec![]);
    let e = explain(&config, "turbovault:read_note", "tasks/x.md");
    assert_eq!(e.verdict, "verdict: BLOCKED");
    assert_eq!(e.fixes.len(), 1);
}

#[test]
fn a_path_addressed_write_without_a_path_cannot_place_its_target() {
    let config = config(
        vec![vault_mcp(Consequence::Reversible)],
        vec![grant(
            "main-agent",
            vec![Capability::ExecuteTool("turbovault:write_note".into())],
        )],
        vec![],
    );
    let e = explain(&config, "turbovault:write_note", "");
    assert!(
        e.guard_lines[1].contains("[BLOCK] write_target"),
        "expected a blocked write_target, got {:?}",
        e.guard_lines[1]
    );
    assert!(e.fixes.iter().any(|f| f.contains("zone_from_arg")));
    assert_eq!(e.verdict, "verdict: BLOCKED");
}

#[test]
fn a_bare_filename_names_no_zone_and_fails_closed() {
    let config = config(
        vec![vault_mcp(Consequence::Reversible)],
        vec![grant(
            "main-agent",
            vec![Capability::ExecuteTool("turbovault:write_note".into())],
        )],
        vec![],
    );
    let e = explain(&config, "turbovault:write_note", "scratch.md");
    assert!(
        e.guard_lines[1].contains("names no zone"),
        "expected the no-zone hint, got {:?}",
        e.guard_lines[1]
    );
    assert_eq!(e.verdict, "verdict: BLOCKED");
}

#[test]
fn a_write_into_an_agent_writable_zone_with_every_grant_is_allowed() {
    let config = config(
        vec![vault_mcp(Consequence::Reversible)],
        vec![grant(
            "main-agent",
            vec![
                Capability::ExecuteTool("turbovault:write_note".into()),
                Capability::Write(Zone::vault("decisions")),
            ],
        )],
        vec![agent_writable("decisions")],
    );
    let e = explain(&config, "turbovault:write_note", "decisions/d.md");
    assert_eq!(
        e.verdict,
        "verdict: ALLOWED — this write would execute directly."
    );
    assert!(e.fixes.is_empty());
    assert!(
        e.guard_lines.iter().all(|l| !l.contains("[BLOCK]")),
        "nothing should block here: {e:?}"
    );
    assert!(
        e.guard_lines
            .iter()
            .any(|l| l.contains("[PASS] write_target      resolves to zone 'decisions'"))
    );
}

#[test]
fn a_write_without_the_zone_capability_accumulates_the_fix() {
    let config = config(
        vec![vault_mcp(Consequence::Reversible)],
        vec![grant(
            "main-agent",
            vec![Capability::ExecuteTool("turbovault:write_note".into())],
        )],
        vec![agent_writable("decisions")],
    );
    let e = explain(&config, "turbovault:write_note", "decisions/d.md");
    assert!(
        e.guard_lines[2].contains("[BLOCK] write_capability"),
        "expected a blocked write_capability, got {:?}",
        e.guard_lines[2]
    );
    assert!(
        e.fixes
            .iter()
            .any(|f| f.contains("Write = { Vault = \"decisions\" }")),
        "expected the write-capability fix, got {e:?}"
    );
    assert_eq!(e.verdict, "verdict: BLOCKED by 1 guard(s):");
}

#[test]
fn an_undeclared_zone_defaults_to_fail_safe_and_says_so() {
    let config = config(
        vec![vault_mcp(Consequence::Reversible)],
        vec![grant(
            "main-agent",
            vec![
                Capability::ExecuteTool("turbovault:write_note".into()),
                Capability::Write(Zone::vault("archive")),
            ],
        )],
        vec![],
    );
    let e = explain(&config, "turbovault:write_note", "archive/a.md");
    assert!(
        e.guard_lines[3].contains("(UNDECLARED — fail-safe default)"),
        "expected the undeclared-zone marker, got {:?}",
        e.guard_lines[3]
    );
    assert!(
        e.fixes
            .iter()
            .any(|f| f.contains("undeclared zones default to proposal_only"))
    );
    assert_eq!(e.verdict, "verdict: BLOCKED by 1 guard(s):");
}

#[test]
fn an_irreversible_mcp_is_gated_even_with_every_grant_in_hand() {
    let config = config(
        vec![vault_mcp(Consequence::External)],
        vec![grant(
            "main-agent",
            vec![
                Capability::ExecuteTool("turbovault:write_note".into()),
                Capability::Write(Zone::vault("decisions")),
            ],
        )],
        vec![agent_writable("decisions")],
    );
    let e = explain(&config, "turbovault:write_note", "decisions/d.md");
    assert!(
        e.guard_lines[4].contains("[BLOCK] consequence"),
        "expected a blocked consequence line, got {:?}",
        e.guard_lines[4]
    );
    assert!(
        e.fixes
            .iter()
            .any(|f| f.contains("proposal-gated by design"))
    );
    assert_eq!(e.verdict, "verdict: BLOCKED by 1 guard(s):");
}
