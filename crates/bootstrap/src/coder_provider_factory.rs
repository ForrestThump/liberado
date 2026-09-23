//! `CoderProviderFactory` for the daemon's coding pack — kept out of `lib.rs` because the
//! factory's `provider_for` body alone has cyclomatic complexity that pushes the rest of the
//! module over its ratchet boundary. Moving it here lets the rest of bootstrap stay below the
//! boundary while the factory does its full job (honour `CoderRoleConfig::model`, route through
//! the shared builder so the fallback wiring matches the daemon path).
//!
//! A fresh provider per call, deliberately. `Provider::set_model` writes through a `RwLock` on
//! the shared trait object, so re-modelling the daemon's provider would change the model for
//! every other holder — the chat face agent included.

use std::sync::Arc;

use super::{build_provider_from_profile, parse_reasoning_level};
use liberado_config::{Config, ProviderProfile};
use liberado_provider::Provider;

/// A `CoderProviderFactory` that honours the model each coding role asks for, and which lives in
/// `liberado-bootstrap` (root) so every composition root that needs to serve a coding role can
/// share the same construction path — instead of each re-deriving `from_env + with_*` locally.
///
/// The pack's own `SingleProviderFactory` returns the one daemon provider for every role,
/// whatever `CoderRoleConfig::model` says — so `[coder.coder].model` selected nothing, and the
/// session event log reported the placeholder `"session-coder"` as the model in use.
pub struct ProfileProviderFactory {
    /// Cloned once at construction; needed at every `provider_for` call to resolve the
    /// configured fallback provider by name (fallback wiring is part of the profile).
    config: Config,
    profile: ProviderProfile,
}

impl ProfileProviderFactory {
    /// `None` when the configured provider has no profile or its API key is unset, so the caller
    /// keeps whatever provider it already had rather than silently losing coding.
    pub fn for_config(config: &Config) -> Option<Self> {
        let profile = super::resolve_provider_profile(config, &config.topology.provider)?.clone();
        std::env::var(&profile.api_key_env).ok()?;
        Some(Self {
            config: config.clone(),
            profile,
        })
    }
}

impl liberado_coder_agent::CoderProviderFactory for ProfileProviderFactory {
    fn provider_for(
        &self,
        _role: &str,
        config: &liberado_coder_core::CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, liberado_coder_core::CoderError> {
        // `None` means the primary API key is missing. A missing fallback key stays on
        // the primary inside `attach_fallback` and still returns `Some`.
        let role_ov = liberado_config::RoleOverride {
            provider: None,
            model: Some(config.model.clone()),
            temperature: config.temperature,
            reasoning: parse_reasoning_level(config.reasoning.as_deref()),
        };
        build_provider_from_profile(&self.config, &self.profile, Some(&role_ov)).ok_or_else(|| {
            liberado_coder_core::CoderError::Backend(format!(
                "API key '{}' is unset for provider '{}'",
                self.profile.api_key_env, self.profile.name
            ))
        })
    }
}
