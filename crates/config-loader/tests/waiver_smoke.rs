//! End-to-end smoke test for `[[risk_waivers]]`: build a `Config`, set waivers directly on its
//! `policy`, run them through `risk_waiver_set()`, and verify the matcher does what an operator
//! expects. Complements the per-type tests in `crates/common/src/risk_waiver.rs` and
//! `crates/config-loader/src/validation.rs` with a single end-to-end path.

use std::path::PathBuf;

use liberado_common::{Guard, RiskWaiver, WriteClass};
use liberado_config_loader::{Config, McpConfig, McpTransport, ZonePolicy};

fn build_test_config() -> Config {
    let mut cfg = Config::default();
    cfg.topology.vault_path = PathBuf::from("/vault");

    cfg.topology.mcps.push(McpConfig {
        name: "liberado-weather-mcp".into(),
        enabled: true,
        description: "weather".into(),
        consequence: liberado_common::Consequence::ReadOnly,
        transport: McpTransport::Http {
            url: "http://x".into(),
        },
        default_zone: None,
        tools: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
        writes_vault: Some(false),
    });

    cfg.topology.mcps.push(McpConfig {
        name: "turbovault".into(),
        enabled: true,
        description: "vault".into(),
        consequence: liberado_common::Consequence::Reversible,
        transport: McpTransport::Http {
            url: "http://x".into(),
        },
        default_zone: None,
        tools: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec!["write_note".into()],
        writes_vault: None,
    });

    cfg.policy.zones = vec![
        ZonePolicy {
            zone: "Tasks".into(),
            write_class: WriteClass::AgentWritable,
        },
        ZonePolicy {
            zone: "Work".into(),
            write_class: WriteClass::AgentWritable,
        },
    ];

    cfg.policy.risk_waivers = vec![
        RiskWaiver {
            mcp: "liberado-weather-mcp".into(),
            match_tools: None,
            match_zones: None,
            guard: Guard::Magnitude,
        },
        RiskWaiver {
            mcp: "turbovault".into(),
            match_tools: Some(vec!["read_note".into()]),
            match_zones: Some(vec!["Tasks".into(), "Work".into()]),
            guard: Guard::Magnitude,
        },
    ];

    cfg
}

#[test]
fn risk_waivers_resolve_at_the_loader_level() {
    let cfg = build_test_config();
    let set = cfg.policy.risk_waiver_set();
    // Wholesale waiver covers any tool on the MCP, with no zone requirement.
    assert!(set.covers(Guard::Magnitude, "liberado-weather-mcp:get_weather", None));
    // Targeted waiver covers read_note on listed zones.
    assert!(set.covers(Guard::Magnitude, "turbovault:read_note", Some("Tasks")));
    assert!(set.covers(Guard::Magnitude, "turbovault:read_note", Some("Work")));
    // Unlisted zones don't match.
    assert!(!set.covers(Guard::Magnitude, "turbovault:read_note", Some("Life")));
    // Unlisted tools don't match.
    assert!(!set.covers(Guard::Magnitude, "turbovault:write_note", Some("Tasks")));
}