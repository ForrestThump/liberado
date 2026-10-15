use std::collections::BTreeSet;

use super::*;
use crate::descriptor::VaultScope;
use crate::effect::Effect;

fn mcp() -> LegacyMcp {
    LegacyMcp {
        server: "turbovault".into(),
        zone_from_arg: None,
        write_tools: Vec::new(),
        read_tools: Vec::new(),
        vault_wide_writes: Vec::new(),
        admin_tools: Vec::new(),
        default_zone: None,
        tool_zones: Vec::new(),
        writes_vault: None,
    }
}

#[test]
fn write_tools_declare_write_and_the_path_argument() {
    let mut mcp = mcp();
    mcp.zone_from_arg = Some("path".into());
    mcp.write_tools = vec!["write_note".into()];
    let policy = policy_from_legacy(&mcp).unwrap();
    let tool = policy.get("turbovault", "write_note").unwrap();
    assert_eq!(tool.effects(), &BTreeSet::from([Effect::Write]));
    assert_eq!(tool.vault(), &VaultScope::from_arg("path"));
}

#[test]
fn read_tools_declare_read() {
    let mut mcp = mcp();
    mcp.zone_from_arg = Some("path".into());
    mcp.read_tools = vec!["read_note".into()];
    let policy = policy_from_legacy(&mcp).unwrap();
    let tool = policy.get("turbovault", "read_note").unwrap();
    assert_eq!(tool.effects(), &BTreeSet::from([Effect::Read]));
    assert_eq!(tool.vault(), &VaultScope::from_arg("path"));
}

#[test]
fn delete_tool_declares_delete() {
    let mut mcp = mcp();
    mcp.zone_from_arg = Some("path".into());
    mcp.write_tools = vec!["delete_note".into(), "tasks_delete".into()];
    let policy = policy_from_legacy(&mcp).unwrap();
    assert_eq!(
        policy.get("turbovault", "delete_note").unwrap().effects(),
        &BTreeSet::from([Effect::Delete])
    );
    assert_eq!(
        policy.get("turbovault", "tasks_delete").unwrap().effects(),
        &BTreeSet::from([Effect::Delete])
    );
}

#[test]
fn undelete_stays_a_write() {
    let mut mcp = mcp();
    mcp.zone_from_arg = Some("path".into());
    mcp.write_tools = vec!["undelete_note".into()];
    let policy = policy_from_legacy(&mcp).unwrap();
    assert_eq!(
        policy.get("turbovault", "undelete_note").unwrap().effects(),
        &BTreeSet::from([Effect::Write])
    );
}

#[test]
fn admin_tools_declare_admin() {
    let mut mcp = mcp();
    mcp.zone_from_arg = Some("path".into());
    mcp.admin_tools = vec!["set_active_vault".into()];
    let policy = policy_from_legacy(&mcp).unwrap();
    let tool = policy.get("turbovault", "set_active_vault").unwrap();
    assert_eq!(tool.effects(), &BTreeSet::from([Effect::Admin]));
    assert_eq!(tool.vault(), &VaultScope::None);
}

#[test]
fn vault_wide_writes_declare_vault_wide_write() {
    let mut mcp = mcp();
    mcp.zone_from_arg = Some("path".into());
    mcp.vault_wide_writes = vec!["vector_reindex".into()];
    let policy = policy_from_legacy(&mcp).unwrap();
    let tool = policy.get("turbovault", "vector_reindex").unwrap();
    assert_eq!(tool.effects(), &BTreeSet::from([Effect::Write]));
    assert_eq!(tool.vault(), &VaultScope::VaultWide);
}

#[test]
fn unlisted_tool_is_absent_and_denied() {
    let mut mcp = mcp();
    mcp.zone_from_arg = Some("path".into());
    mcp.write_tools = vec!["write_note".into()];
    mcp.read_tools = vec!["read_note".into()];
    let policy = policy_from_legacy(&mcp).unwrap();
    assert!(policy.get("turbovault", "edit_note").is_none());
    let error = policy
        .authorize(&crate::ToolCall {
            server: "turbovault".into(),
            tool: "edit_note".into(),
            effect: Effect::Write,
            vault_paths: vec!["tasks/a.md".into()],
            urls: Vec::new(),
            external_services: Vec::new(),
        })
        .unwrap_err();
    assert!(matches!(error, crate::Deny::Undeclared { .. }));
}

#[test]
fn overlapping_lists_are_refused() {
    let mut mcp = mcp();
    mcp.zone_from_arg = Some("path".into());
    mcp.write_tools = vec!["write_note".into()];
    mcp.read_tools = vec!["write_note".into()];
    let error = policy_from_legacy(&mcp).unwrap_err();
    assert_eq!(
        error,
        CompatError::Overlap {
            name: "write_note".into()
        }
    );
}

#[test]
fn fixed_default_zone_is_the_write_zone() {
    let mut mcp = mcp();
    mcp.default_zone = Some("finance".into());
    mcp.write_tools = vec!["charge".into()];
    let policy = policy_from_legacy(&mcp).unwrap();
    let tool = policy.get("turbovault", "charge").unwrap();
    assert_eq!(tool.effects(), &BTreeSet::from([Effect::Write]));
    assert_eq!(tool.vault(), &VaultScope::zones(vec!["finance".into()]));
}

#[test]
fn explicit_empty_tool_zone_is_a_read() {
    let mut mcp = mcp();
    mcp.default_zone = Some("finance".into());
    mcp.tool_zones = vec![LegacyToolZone {
        name: "get_balance".into(),
        zone: None,
    }];
    let policy = policy_from_legacy(&mcp).unwrap();
    let tool = policy.get("turbovault", "get_balance").unwrap();
    assert_eq!(tool.effects(), &BTreeSet::from([Effect::Read]));
    assert_eq!(tool.vault(), &VaultScope::None);
}

#[test]
fn writes_vault_false_keeps_write_off_the_vault() {
    let mut mcp = mcp();
    mcp.writes_vault = Some(false);
    mcp.write_tools = vec!["render_pdf".into()];
    let policy = policy_from_legacy(&mcp).unwrap();
    let tool = policy.get("turbovault", "render_pdf").unwrap();
    assert_eq!(tool.effects(), &BTreeSet::from([Effect::Write]));
    assert_eq!(tool.vault(), &VaultScope::None);
}

#[test]
fn unplaced_write_is_refused() {
    let mut mcp = mcp();
    mcp.write_tools = vec!["write_note".into()];
    let error = policy_from_legacy(&mcp).unwrap_err();
    assert_eq!(
        error,
        CompatError::UnplacedWrite {
            name: "write_note".into(),
        }
    );
}

#[test]
fn path_addressed_and_fixed_zone_together_are_refused() {
    let mut mcp = mcp();
    mcp.zone_from_arg = Some("path".into());
    mcp.default_zone = Some("tasks".into());
    mcp.write_tools = vec!["write_note".into()];
    let error = policy_from_legacy(&mcp).unwrap_err();
    assert_eq!(error, CompatError::MixedStyles);
}
