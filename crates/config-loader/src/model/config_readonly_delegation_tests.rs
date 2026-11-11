//! A read-only profile that omits `delegation` resolves to off.

use liberado_common::{Capability, Zone};

use super::super::policy::Grant;
use super::super::topology::SessionProfile;
use super::Config;

fn research_config(delegation: Option<bool>, write: bool) -> Config {
    let mut cfg = Config::default();
    cfg.topology.vault_path = std::path::PathBuf::from("/tmp/vault");
    let mut profile = SessionProfile::empty("researcher");
    profile.component = Some("research".into());
    profile.delegation = delegation;
    profile.read = vec!["tasks".into()];
    if write {
        profile.write = vec!["tasks".into()];
    }
    cfg.topology.session_profiles = vec![profile];
    cfg.policy.grants = vec![Grant {
        component: "research".into(),
        capabilities: vec![Capability::Read(Zone::vault("tasks"))],
    }];
    cfg
}

#[test]
fn a_read_only_profile_with_no_delegation_key_resolves_off() {
    let cfg = research_config(None, false);
    let resolved = cfg
        .resolve_session_profile(Some("researcher"), "dispatch")
        .expect("researcher resolves");
    assert_eq!(
        resolved.delegation,
        Some(false),
        "a read-only grant must not inherit delegation"
    );
    assert!(resolved.capabilities.authorizes_only_reads());
}

#[test]
fn an_explicit_delegation_flag_wins_on_a_read_only_profile() {
    let cfg = research_config(Some(true), false);
    let resolved = cfg
        .resolve_session_profile(Some("researcher"), "dispatch")
        .expect("researcher resolves");
    assert_eq!(resolved.delegation, Some(true));
}

#[test]
fn a_writing_profile_that_omits_delegation_still_inherits() {
    let cfg = research_config(None, true);
    let resolved = cfg
        .resolve_session_profile(Some("researcher"), "dispatch")
        .expect("researcher resolves");
    assert_eq!(resolved.delegation, None);
    assert!(!resolved.capabilities.authorizes_only_reads());
}

#[test]
fn an_empty_grant_that_omits_delegation_still_inherits() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = std::path::PathBuf::from("/tmp/vault");
    cfg.topology.session_profiles = vec![SessionProfile::empty("quiet")];
    let resolved = cfg
        .resolve_session_profile(Some("quiet"), "dispatch")
        .expect("quiet resolves");
    assert_eq!(resolved.delegation, None);
    assert!(!resolved.capabilities.authorizes_only_reads());
}

#[test]
fn a_policy_read_grant_with_no_delegation_key_resolves_off() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = std::path::PathBuf::from("/tmp/vault");
    let mut profile = SessionProfile::empty("researcher");
    profile.component = Some("research".into());
    cfg.topology.session_profiles = vec![profile];
    cfg.policy.grants = vec![Grant {
        component: "research".into(),
        capabilities: vec![Capability::Read(Zone::vault("tasks"))],
    }];
    let resolved = cfg
        .resolve_session_profile(Some("researcher"), "dispatch")
        .expect("researcher resolves");
    assert_eq!(resolved.delegation, Some(false));
    assert!(resolved.capabilities.authorizes_only_reads());
}

/// `AskHuman` is not a read. The coding hat (read + ask) still inherits.
#[test]
fn a_read_plus_ask_human_profile_still_inherits_delegation() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = std::path::PathBuf::from("/tmp/vault");
    let mut profile = SessionProfile::empty("coding");
    profile.component = Some("coding".into());
    cfg.topology.session_profiles = vec![profile];
    cfg.policy.grants = vec![Grant {
        component: "coding".into(),
        capabilities: vec![
            Capability::Read(Zone::vault("decisions")),
            Capability::AskHuman,
        ],
    }];
    let resolved = cfg
        .resolve_session_profile(Some("coding"), "dispatch")
        .expect("coding resolves");
    assert_eq!(resolved.delegation, None);
    assert!(!resolved.capabilities.authorizes_only_reads());
}
