//! Cross-provider fallback wiring.
//!
//! Kept out of `lib.rs` so the fallback complexity doesn't bleed into the cyclomatic-complexity
//! ratchet for the rest of the bootstrap module. The wiring has three failure modes the caller
//! needs to know about (none of which panic):
//!
//! 1. The fallback name isn't declared in `topology.providers`. Validation should have caught
//!    this at config load; if it slipped through, we warn and return the primary unwired.
//! 2. The fallback provider's API key is unset in the environment. We warn and return the
//!    primary unwired — the daemon still runs on the primary, the fallback just doesn't fire.
//! 3. Everything's fine. We return the primary with the fallback attached via
//!    [`OpenAiCompatibleProvider::with_fallback`].
//!
//! In all three cases the caller gets back an `OpenAiCompatibleProvider` ready to wrap in an
//! `Arc<dyn Provider>`; the fallback wiring or its absence is invisible at the
//! `Provider`-trait boundary.

use liberado_config::{Config, ProviderFallback, ProviderProfile};
use liberado_provider::Provider;
use liberado_provider_openai_compat::OpenAiCompatibleProvider;

/// Attach the configured fallback (if any) to `primary`. See the module doc comment for the
/// failure modes.
pub fn attach_fallback(
    config: &Config,
    primary_profile: &ProviderProfile,
    primary: OpenAiCompatibleProvider,
) -> OpenAiCompatibleProvider {
    let Some(fb_cfg) = primary_profile.fallback.as_ref() else {
        return primary;
    };
    let Some(fb_profile) = config
        .topology
        .providers
        .iter()
        .find(|p| p.name == fb_cfg.provider)
    else {
        warn_undeclared(primary_profile, fb_cfg);
        return primary;
    };
    build_and_attach(primary_profile, fb_profile, fb_cfg, primary)
}

fn build_and_attach(
    primary_profile: &ProviderProfile,
    fb_profile: &ProviderProfile,
    fb_cfg: &ProviderFallback,
    primary: OpenAiCompatibleProvider,
) -> OpenAiCompatibleProvider {
    let fb_provider = match OpenAiCompatibleProvider::from_env(
        &fb_profile.api_key_env,
        fb_profile.model_env.as_deref(),
        &fb_profile.default_model,
        &fb_profile.base_url,
        fb_profile.extra_client_error_status.clone(),
    ) {
        Ok(p) => p,
        Err(_) => {
            warn_missing_key(primary_profile, fb_cfg);
            return primary;
        }
    };
    // Apply the override model if one was declared on the fallback config (the only way to
    // pin a model on the fallback that differs from that provider's default).
    if let Some(m) = fb_cfg.model.as_deref() {
        fb_provider.set_model(m.to_string());
    }
    primary.with_fallback(fb_provider, fb_cfg.on_status.clone())
}

fn warn_undeclared(primary_profile: &ProviderProfile, fb_cfg: &ProviderFallback) {
    tracing::warn!(
        primary_provider = %primary_profile.name,
        fallback_provider = %fb_cfg.provider,
        "fallback provider name is not declared in topology.providers — primary will run without fallback"
    );
}

fn warn_missing_key(primary_profile: &ProviderProfile, fb_cfg: &ProviderFallback) {
    tracing::warn!(
        primary_provider = %primary_profile.name,
        fallback_provider = %fb_cfg.provider,
        "fallback provider configured but its API key is unset in the environment — \
         the primary will run without fallback"
    );
}

/// Build a fallback provider for `profile` standalone, applying an optional model override.
/// Used by composition roots that need to construct a fallback outside `attach_fallback`'s
/// config-driven shape (none today, but kept here so future callers don't reach back into
/// `attach_fallback` internals).
#[allow(dead_code)]
pub fn build_fallback_provider(
    profile: &ProviderProfile,
    model_override: Option<&str>,
) -> Option<OpenAiCompatibleProvider> {
    let p = OpenAiCompatibleProvider::from_env(
        &profile.api_key_env,
        profile.model_env.as_deref(),
        &profile.default_model,
        &profile.base_url,
        profile.extra_client_error_status.clone(),
    )
    .ok()?;
    if let Some(m) = model_override {
        p.set_model(m.to_string());
    }
    Some(p)
}

// Re-export `Arc` so callers of this module don't need a second import. Tiny, but the file is
// already opt-in and the upstream uses `Arc<dyn Provider>` everywhere.
// (No re-export needed: callers in `lib.rs` already import `Arc` directly.)
