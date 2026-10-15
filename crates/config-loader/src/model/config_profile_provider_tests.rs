//! Load-time checks for `[[session_profiles]].provider`.

use std::path::PathBuf;

use super::super::policy::Grant;
use super::super::topology::SessionProfile;
use super::Config;

fn config_with(provider: Option<&str>) -> Config {
    let mut cfg = Config::default();
    cfg.topology.vault_path = PathBuf::from("/tmp/vault");
    let mut profile = SessionProfile::empty("dogfood");
    profile.provider = provider.map(str::to_string);
    cfg.topology.session_profiles = vec![profile];
    cfg.policy.grants.push(Grant {
        component: "dogfood".into(),
        capabilities: Vec::new(),
    });
    cfg
}

#[test]
fn a_profile_provider_parses_and_an_absent_one_stays_unset() {
    let named: SessionProfile = toml::from_str(
        r#"
        name = "dogfood"
        model = "deepseek/deepseek-v4-flash"
        provider = "openrouter"
        "#,
    )
    .expect("documented shape");
    assert_eq!(named.provider.as_deref(), Some("openrouter"));
    assert_eq!(named.model.as_deref(), Some("deepseek/deepseek-v4-flash"));

    let plain: SessionProfile = toml::from_str(r#"name = "plain""#).expect("name only");
    assert!(plain.provider.is_none());
}

#[test]
fn a_known_provider_loads_and_an_omitted_one_keeps_today() {
    assert!(
        config_with(Some("openrouter")).validate().is_ok(),
        "openrouter is a default [[providers]] entry"
    );
    assert!(
        config_with(None).validate().is_ok(),
        "no provider field is the daemon default"
    );
}

#[test]
fn an_unknown_provider_id_is_a_load_error() {
    let err = config_with(Some("minimax"))
        .validate()
        .expect_err("unknown id");
    let msg = err.to_string();
    assert!(msg.contains("minimax"), "{msg}");
    assert!(msg.contains("does not name"), "{msg}");
}

#[test]
fn an_empty_provider_id_is_a_load_error() {
    let err = config_with(Some("  ")).validate().expect_err("blank id");
    let msg = err.to_string();
    assert!(msg.contains("empty"), "{msg}");
}

#[test]
fn a_provider_with_no_credentials_config_is_a_load_error() {
    let mut cfg = config_with(Some("openrouter"));
    for provider in &mut cfg.topology.providers {
        if provider.name == "openrouter" {
            provider.api_key_env.clear();
        }
    }
    let err = cfg.validate().expect_err("empty api_key_env");
    let msg = err.to_string();
    assert!(msg.contains("no credentials/config"), "{msg}");
    assert!(msg.contains("openrouter"), "{msg}");
}

#[test]
fn a_disabled_profile_may_name_an_unknown_provider() {
    let mut cfg = config_with(Some("minimax"));
    cfg.topology.session_profiles[0].enabled = false;
    assert!(
        cfg.validate().is_ok(),
        "disabled profiles are drafts, same as the grant check"
    );
}
