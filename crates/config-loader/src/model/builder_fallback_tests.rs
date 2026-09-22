//! Validation tests for the `[[providers.fallback]]` schema.
//!
//! Sibling test file (via `#[path]` in `builder.rs`) so the fallback-specific test growth
//! doesn't trip the module-health ratchet on `builder.rs`. The tests cover:
//!
//! - unknown fallback provider name → load-time error
//! - self-reference → load-time error (would loop on every fallback-eligible failure)
//! - well-formed fallback → loads cleanly

use std::path::PathBuf;

use crate::model::config::Config;
use crate::model::topology::ProviderFallback;

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
