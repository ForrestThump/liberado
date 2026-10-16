//! The editor hat resolves with no topology row. A stranger name still fails.

use liberado_common::{Capability, Zone};

use super::super::config::Config;
use super::super::policy::Grant;
use super::super::topology::SessionProfile;
use super::BUILTIN_AUTOMATION_EDITOR_PROFILE;

fn config_with_editor_policy_only() -> Config {
    let mut cfg = Config::default();
    cfg.topology.vault_path = std::path::PathBuf::from("/tmp/vault");
    cfg.policy.grants = vec![
        Grant {
            component: "main-agent".into(),
            capabilities: vec![Capability::AskHuman],
        },
        Grant {
            component: BUILTIN_AUTOMATION_EDITOR_PROFILE.into(),
            capabilities: vec![Capability::ExecuteTool(
                "automation-editor:list_schedules".into(),
            )],
        },
    ];
    cfg
}

#[test]
fn editor_hat_resolves_without_a_topology_row_and_a_stranger_name_does_not() {
    let cfg = config_with_editor_policy_only();
    assert!(
        cfg.topology
            .session_profiles
            .iter()
            .all(|profile| profile.name != BUILTIN_AUTOMATION_EDITOR_PROFILE),
        "the fixture must not list the editor hat"
    );
    assert!(
        !cfg.tuning
            .chat
            .agent_profiles
            .iter()
            .any(|name| name == BUILTIN_AUTOMATION_EDITOR_PROFILE),
        "the editor hat is not an Agents-shelf profile"
    );

    let resolved = cfg
        .resolve_session_profile(Some(BUILTIN_AUTOMATION_EDITOR_PROFILE), "main-agent")
        .expect("the built-in editor hat resolves with no topology row");
    let listed = Capability::ExecuteTool("automation-editor:list_schedules".into());
    assert_eq!(
        resolved.name.as_deref(),
        Some(BUILTIN_AUTOMATION_EDITOR_PROFILE)
    );
    assert!(resolved.domain.is_none());
    assert_eq!(resolved.delegation, Some(false));
    assert!(resolved.model.is_none());
    assert!(resolved.prompt_append.is_none());
    assert!(matches!(resolved.overrides, toml::Value::Table(ref table) if table.is_empty()));
    assert_eq!(resolved.capabilities.capabilities, vec![listed]);
    assert!(!resolved.capabilities.grants_ask_human());
    assert!(
        !resolved
            .capabilities
            .contains(&Capability::Read(Zone::vault("Work"))),
        "the editor hat must not pick up a vault read the policy grant omitted"
    );

    let err = cfg
        .resolve_session_profile(Some("not-a-real-hat"), "main-agent")
        .expect_err("an unknown name stays an error");
    let text = err.to_string();
    assert!(text.contains("not-a-real-hat"), "{text}");
    assert!(text.contains("does not name an enabled"), "{text}");
}

#[test]
fn a_disabled_editor_row_is_not_replaced_by_the_builtin() {
    let mut cfg = config_with_editor_policy_only();
    let mut row = SessionProfile::empty(BUILTIN_AUTOMATION_EDITOR_PROFILE);
    row.enabled = false;
    cfg.topology.session_profiles = vec![row];
    let err = cfg
        .resolve_session_profile(Some(BUILTIN_AUTOMATION_EDITOR_PROFILE), "main-agent")
        .expect_err("a disabled row stays fail-closed");
    assert!(
        err.to_string().contains(BUILTIN_AUTOMATION_EDITOR_PROFILE),
        "{err}"
    );
}
