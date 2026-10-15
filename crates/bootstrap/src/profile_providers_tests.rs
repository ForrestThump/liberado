//! Wiring of `[[session_profiles]].provider` when the API key is absent.

use liberado_config::{Config, ProviderProfile, SessionProfile};

use super::profile_providers_from_config;

fn routed_config() -> Config {
    let mut cfg = Config::default();
    cfg.topology.providers.push(ProviderProfile {
        name: "routed".into(),
        base_url: "https://example.test/v1".into(),
        default_model: "deepseek/deepseek-v4-flash".into(),
        api_key_env: "LIBERADO_PROFILE_ROUTE_KEY_UNSET".into(),
        model_env: None,
        extra_client_error_status: Vec::new(),
        fallback: None,
    });
    let mut profile = SessionProfile::empty("dogfood");
    profile.provider = Some("routed".into());
    cfg.topology.session_profiles = vec![profile];
    cfg
}

#[test]
fn an_unset_api_key_marks_the_profile_provider_unavailable() {
    let wired = profile_providers_from_config(&routed_config());
    assert_eq!(
        wired.routes.get("dogfood").map(String::as_str),
        Some("routed")
    );
    assert!(
        !wired.backends.contains_key("routed"),
        "a missing key must not build a client"
    );
    let reason = wired
        .unavailable
        .get("routed")
        .expect("missing key is recorded");
    assert!(
        reason.contains("LIBERADO_PROFILE_ROUTE_KEY_UNSET"),
        "{reason}"
    );
}

#[test]
fn a_profile_without_provider_adds_no_route() {
    let mut cfg = Config::default();
    cfg.topology.session_profiles = vec![SessionProfile::empty("plain")];
    let wired = profile_providers_from_config(&cfg);
    assert!(wired.routes.is_empty());
    assert!(wired.unavailable.is_empty());
}
