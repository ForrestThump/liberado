//! Load-time checks for model ids that name a provider by shape, not by catalog.

use std::path::PathBuf;

use liberado_common::ModelRole;

use super::super::policy::Grant;
use super::super::topology::{ProviderFallback, ProviderProfile, RoleOverride, SessionProfile};
use super::Config;

const OPENROUTER_SLUG: &str = "deepseek/deepseek-v4-flash";

fn bare_config() -> Config {
    let mut cfg = Config::default();
    cfg.topology.vault_path = PathBuf::from("/tmp/vault");
    cfg
}

fn provider(name: &str, default_model: &str) -> ProviderProfile {
    ProviderProfile {
        name: name.into(),
        base_url: format!("https://{name}.example/v1"),
        default_model: default_model.into(),
        api_key_env: format!("{name}_KEY"),
        model_env: None,
        extra_client_error_status: Vec::new(),
        fallback: None,
    }
}

fn with_profile(model: Option<&str>, provider_id: Option<&str>) -> Config {
    let mut cfg = bare_config();
    let mut profile = SessionProfile::empty("budget-bot");
    profile.model = model.map(str::to_string);
    profile.provider = provider_id.map(str::to_string);
    cfg.policy.grants.push(Grant {
        component: "budget-bot".into(),
        capabilities: Vec::new(),
    });
    cfg.topology.session_profiles = vec![profile];
    cfg
}

fn production_providers() -> Vec<ProviderProfile> {
    vec![
        provider("minimax", "MiniMax-M3"),
        provider("openrouter", "openai/gpt-4o-mini"),
    ]
}

#[test]
fn a_vendor_model_id_on_the_default_direct_provider_fails_load() {
    let err = with_profile(Some(OPENROUTER_SLUG), None)
        .validate()
        .expect_err("deepseek's default model is a bare slug")
        .to_string();
    assert!(err.contains("budget-bot"), "{err}");
    assert!(err.contains(OPENROUTER_SLUG), "{err}");
    assert!(err.contains("deepseek"), "{err}");
    assert!(err.contains("openrouter"), "{err}");
    assert!(err.contains("Providers considered"), "{err}");
    assert!(err.contains("list_models"), "{err}");
    assert!(
        err.contains("matches the selected provider"),
        "the prefix matches deepseek, and that is who would receive the id: {err}"
    );
}

#[test]
fn the_same_slug_is_accepted_when_the_profile_names_openrouter() {
    assert!(
        with_profile(Some(OPENROUTER_SLUG), Some("openrouter"))
            .validate()
            .is_ok(),
        "openrouter's default model is itself a vendor/model id"
    );
}

#[test]
fn an_openrouter_slug_is_not_sent_to_a_provider_named_deepseek() {
    let mut cfg = with_profile(Some(OPENROUTER_SLUG), None);
    cfg.topology.provider = "minimax".into();
    cfg.topology.providers = production_providers();
    cfg.topology
        .providers
        .push(provider("deepseek", "deepseek-chat"));
    let err = cfg
        .validate()
        .expect_err("minimax cannot be shown to serve the slug")
        .to_string();
    assert!(err.contains("budget-bot"), "{err}");
    assert!(err.contains(OPENROUTER_SLUG), "{err}");
    assert!(err.contains("would be sent to provider 'minimax'"), "{err}");
    assert!(err.contains("MiniMax-M3"), "{err}");
    assert!(err.contains("openai/gpt-4o-mini"), "{err}");
    assert!(
        err.contains("Prefixes are not used as routes"),
        "deepseek is configured, but the prefix must not retarget the call: {err}"
    );
}

#[test]
fn a_bare_slug_is_not_checked_against_a_catalog() {
    assert!(
        with_profile(Some("MiniMax-M3"), None).validate().is_ok(),
        "a bare id may be valid on the receiving provider; list_models is live"
    );
}

#[test]
fn an_empty_profile_model_is_a_load_error() {
    let err = with_profile(Some("  "), None)
        .validate()
        .expect_err("blank model")
        .to_string();
    assert!(err.contains("budget-bot"), "{err}");
    assert!(err.contains("empty"), "{err}");
}

#[test]
fn a_disabled_profile_may_pin_an_unservable_model() {
    let mut cfg = with_profile(Some(OPENROUTER_SLUG), None);
    cfg.topology.session_profiles[0].enabled = false;
    assert!(
        cfg.validate().is_ok(),
        "disabled profiles are drafts, same as the provider check"
    );
}

#[test]
fn a_role_model_uses_the_same_shape_rule() {
    let mut cfg = bare_config();
    cfg.topology.roles.insert(
        ModelRole::Dispatcher,
        RoleOverride {
            model: Some(OPENROUTER_SLUG.into()),
            ..RoleOverride::default()
        },
    );
    let err = cfg
        .validate()
        .expect_err("dispatcher inherits deepseek")
        .to_string();
    assert!(err.contains("topology.roles.dispatcher.model"), "{err}");
    assert!(err.contains(OPENROUTER_SLUG), "{err}");
    assert!(err.contains("Providers considered"), "{err}");

    cfg.topology
        .roles
        .get_mut(&ModelRole::Dispatcher)
        .unwrap()
        .provider = Some("openrouter".into());
    assert!(
        cfg.validate().is_ok(),
        "an explicit openrouter role may pin a vendor/model id"
    );
}

#[test]
fn a_role_model_that_names_an_unknown_provider_fails_load() {
    let mut cfg = bare_config();
    cfg.topology.roles.insert(
        ModelRole::MainAgent,
        RoleOverride {
            provider: Some("typo".into()),
            model: Some("MiniMax-M3".into()),
            ..RoleOverride::default()
        },
    );
    let err = cfg
        .validate()
        .expect_err("unknown role provider")
        .to_string();
    assert!(err.contains("topology.roles.main_agent.model"), "{err}");
    assert!(err.contains("typo"), "{err}");
    assert!(err.contains("Providers considered"), "{err}");
    assert!(err.contains("deepseek"), "{err}");
}

#[test]
fn a_fallback_model_must_fit_the_fallback_provider() {
    let mut cfg = bare_config();
    cfg.topology.provider = "minimax".into();
    let mut minimax = provider("minimax", "MiniMax-M3");
    minimax.fallback = Some(ProviderFallback {
        provider: "openrouter".into(),
        model: Some(OPENROUTER_SLUG.into()),
        on_status: vec![402],
    });
    cfg.topology.providers = vec![minimax, provider("openrouter", "openai/gpt-4o-mini")];
    assert!(
        cfg.validate().is_ok(),
        "the documented MiniMax → OpenRouter fallback pins a vendor/model id on OpenRouter"
    );

    cfg.topology.providers[0]
        .fallback
        .as_mut()
        .unwrap()
        .provider = "direct".into();
    cfg.topology
        .providers
        .push(provider("direct", "plain-slug"));
    let err = cfg
        .validate()
        .expect_err("a direct fallback provider cannot take a vendor/model id")
        .to_string();
    assert!(
        err.contains("topology.providers['minimax'].fallback.model"),
        "{err}"
    );
    assert!(err.contains("would be sent to provider 'direct'"), "{err}");
    assert!(err.contains(OPENROUTER_SLUG), "{err}");
}

#[test]
fn a_leading_slash_does_not_invent_a_provider_prefix() {
    let err = with_profile(Some("/no-vendor"), None)
        .validate()
        .expect_err("slash id on deepseek")
        .to_string();
    assert!(err.contains("/no-vendor"), "{err}");
    assert!(
        !err.contains("matches"),
        "an empty prefix is not a provider name: {err}"
    );
}
