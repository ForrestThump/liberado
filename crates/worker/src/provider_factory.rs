//! The provider factory behind delegated runs: one topology profile, OpenAI-compatible
//! wire. Mirrors `liberado-coder-run`'s factory — same construction, same missing-key
//! error at assembly time rather than three turns in.

use std::env;
use std::sync::Arc;

use liberado_coder_agent::CoderProviderFactory;
use liberado_coder_core::{CoderError, CoderRoleConfig};
use liberado_config_loader::ProviderProfile;
use liberado_provider::Provider;
use liberado_provider_openai_compat::OpenAiCompatibleProvider;

#[derive(Debug, Clone)]
pub struct ProfileProviderFactory {
    profile: ProviderProfile,
    api_key: String,
}

impl ProfileProviderFactory {
    pub fn from_profile(profile: ProviderProfile) -> Result<Self, String> {
        let api_key = env::var(&profile.api_key_env).map_err(|_| {
            format!(
                "{} is required for provider '{}'",
                profile.api_key_env, profile.name
            )
        })?;
        Ok(Self { profile, api_key })
    }
}

impl CoderProviderFactory for ProfileProviderFactory {
    fn provider_for(
        &self,
        _role: &str,
        config: &CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, CoderError> {
        let provider =
            OpenAiCompatibleProvider::new(&self.api_key, &config.model, &self.profile.base_url)
                .with_extra_client_error_status(self.profile.extra_client_error_status.clone())
                .with_reasoning_effort(config.reasoning.clone());
        Ok(Arc::new(provider))
    }
}
