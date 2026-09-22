//! Validation tests for the `[providers.fallback]` schema.
//!
//! Sibling test file (via `#[path]` in `builder.rs`) so the fallback-specific test growth
//! doesn't trip the module-health ratchet on `builder.rs`. The tests cover:
//!
//! - unknown fallback provider name → load-time error
//! - self-reference → load-time error (would loop on every fallback-eligible failure)
//! - well-formed fallback → loads cleanly

use std::path::PathBuf;

use crate::model::config::Config;
use crate::model::topology::{ProviderFallback, Topology};

use super::provider_profile;

#[test]
fn fallback_to_an_undeclared_provider_fails_validation() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
    cfg.topology.providers = vec![provider_profile("deepseek")];
    let deepseek = &mut cfg.topology.providers[0];
    deepseek.fallback = Some(ProviderFallback {
        provider: "ghost-backend".into(),
        model: None,
        on_status: vec![402],
    });
    let err = cfg
        .validate()
        .expect_err("undeclared fallback must fail validation");
    assert!(err.to_string().contains("ghost-backend"), "{err}");
}

#[test]
fn self_fallback_fails_validation_because_it_would_loop() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
    cfg.topology.providers = vec![provider_profile("deepseek")];
    cfg.topology.providers[0].fallback = Some(ProviderFallback {
        provider: "deepseek".into(), // points at itself
        model: None,
        on_status: vec![402],
    });
    let err = cfg
        .validate()
        .expect_err("self-fallback must fail validation");
    assert!(
        err.to_string()
            .contains("cannot be the same as the primary"),
        "{err}"
    );
}

#[test]
fn fallback_to_a_declared_provider_validates_cleanly() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
    cfg.topology.providers = vec![provider_profile("deepseek"), provider_profile("openrouter")];
    cfg.topology.providers[0].fallback = Some(ProviderFallback {
        provider: "openrouter".into(),
        model: Some("deepseek/deepseek-v4-flash-0731".into()),
        on_status: vec![402, 429, 500],
    });
    assert!(cfg.validate().is_ok(), "well-formed fallback must validate");
}

const FALLBACK_SNIPPET: &str = r#"
vault_path = "/tmp/vault"

[[providers]]
name = "minimax"
base_url = "https://api.minimax.io/v1"
default_model = "MiniMax-M3"
api_key_env = "MINIMAX_API_KEY"

[providers.fallback]
provider = "openrouter"
model = "deepseek/deepseek-v4-flash-0731"
on_status = [402]

[[providers]]
name = "openrouter"
base_url = "https://openrouter.ai/api/v1"
default_model = "openai/gpt-4o-mini"
api_key_env = "OPENROUTER_API_KEY"
"#;

#[test]
fn dotted_fallback_table_deserializes_onto_the_provider() {
    let topology: Topology = toml::from_str(FALLBACK_SNIPPET).expect("dotted table must parse");
    let fb = topology.providers[0]
        .fallback
        .as_ref()
        .expect("fallback is present");
    assert_eq!(fb.provider, "openrouter");
    assert_eq!(fb.model.as_deref(), Some("deepseek/deepseek-v4-flash-0731"));
    assert!(topology.providers[1].fallback.is_none());
}

#[test]
fn array_of_tables_fallback_header_does_not_deserialize() {
    let toml = FALLBACK_SNIPPET.replace("[providers.fallback]", "[[providers.fallback]]");
    let err = toml::from_str::<Topology>(&toml).expect_err("array header must not parse");
    let msg = err.to_string();
    assert!(
        msg.contains("invalid type"),
        "expected a sequence-versus-struct error, got: {msg}"
    );
}
