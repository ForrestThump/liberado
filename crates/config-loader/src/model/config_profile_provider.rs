//! `[[session_profiles]].provider` checks, kept off `config.rs`.
//!
//! An unknown id, a blank id, or a provider entry with an empty `base_url` or
//! `api_key_env` fails load. That matches the other dangling-reference checks in
//! [`Config::validate`](super::Config::validate), and it does not read the process
//! environment. An unset API-key variable is not a load error: the default provider
//! already boots the daemon without one (watch-only), and failing the whole load
//! would take down every profile. Bootstrap records that case and the turn fails
//! with the variable name, instead of sending the slug to the default backend.

use liberado_common::{Error, Result};

use super::config::Config;
use super::topology::{ProviderProfile, SessionProfile};

impl Config {
    pub(super) fn validate_profile_providers(&self) -> Result<()> {
        for profile in &self.topology.session_profiles {
            if profile.enabled {
                validate_one(self, profile)?;
            }
        }
        Ok(())
    }
}

fn validate_one(config: &Config, profile: &SessionProfile) -> Result<()> {
    let Some(raw) = profile.provider.as_deref() else {
        return Ok(());
    };
    let id = raw.trim();
    if id.is_empty() {
        return Err(Error::Config(format!(
            "topology.session_profiles['{}'].provider is empty — omit the key to use the \
             default provider, or name a topology.providers entry",
            profile.name
        )));
    }
    let Some(declared) = find_provider(config, id) else {
        return Err(Error::Config(format!(
            "topology.session_profiles['{}'].provider '{id}' does not name a \
             topology.providers entry",
            profile.name
        )));
    };
    if declared.base_url.trim().is_empty() || declared.api_key_env.trim().is_empty() {
        return Err(Error::Config(format!(
            "topology.session_profiles['{}'].provider '{id}' has no credentials/config \
             (base_url and api_key_env must both be non-empty)",
            profile.name
        )));
    }
    Ok(())
}

fn find_provider<'a>(config: &'a Config, id: &str) -> Option<&'a ProviderProfile> {
    config
        .topology
        .providers
        .iter()
        .find(|provider| provider.name == id)
}

#[cfg(test)]
#[path = "config_profile_provider_tests.rs"]
mod tests;
