//! Build the backends named by `[[session_profiles]].provider`.
//!
//! Missing API keys are not a config-load error. `Config::validate` does not read
//! the environment, and a missing key on the default provider already boots the
//! daemon watch-only. Failing the whole load here would stop every other profile.
//! The id is recorded in [`ProfileProviderWiring::unavailable`] and the turn fails
//! with that reason, instead of sending the profile's model to the default backend.

use std::collections::HashMap;
use std::sync::Arc;

use liberado_config::Config;
use liberado_provider::Provider;

/// Profile name → provider id, plus the backends that could be built.
#[derive(Default)]
pub struct ProfileProviderWiring {
    /// Enabled profile name → provider id. Profiles that omit `provider` are absent.
    pub routes: HashMap<String, String>,
    /// Provider id → client. Only ids whose API key was set.
    pub backends: HashMap<String, Arc<dyn Provider>>,
    /// Provider id → why it was not built. The turn surfaces this text.
    pub unavailable: HashMap<String, String>,
}

/// One pass over enabled session profiles. Disabled profiles are drafts and are skipped,
/// matching the grant check in `Config::validate_session_profiles`.
pub fn profile_providers_from_config(config: &Config) -> ProfileProviderWiring {
    let mut wired = ProfileProviderWiring::default();
    for profile in &config.topology.session_profiles {
        if !profile.enabled {
            continue;
        }
        let Some(raw) = profile.provider.as_deref() else {
            continue;
        };
        let id = raw.trim();
        if id.is_empty() {
            continue;
        }
        record_route(config, &mut wired, &profile.name, id);
    }
    wired
}

fn record_route(config: &Config, wired: &mut ProfileProviderWiring, profile: &str, id: &str) {
    wired.routes.insert(profile.to_string(), id.to_string());
    if wired.backends.contains_key(id) || wired.unavailable.contains_key(id) {
        return;
    }
    let Some(declared) = find_provider(config, id) else {
        let reason = format!("no config: '{id}' is not a topology.providers entry");
        warn_unavailable(profile, id, &reason);
        wired.unavailable.insert(id.to_string(), reason);
        return;
    };
    match super::build_provider_from_profile(config, declared, None) {
        Some(provider) => {
            wired.backends.insert(id.to_string(), provider);
        }
        None => {
            let reason = format!(
                "no credentials: environment variable '{}' is unset",
                declared.api_key_env
            );
            warn_unavailable(profile, id, &reason);
            wired.unavailable.insert(id.to_string(), reason);
        }
    }
}

fn find_provider<'a>(config: &'a Config, id: &str) -> Option<&'a liberado_config::ProviderProfile> {
    config
        .topology
        .providers
        .iter()
        .find(|provider| provider.name == id)
}

fn warn_unavailable(profile: &str, id: &str, reason: &str) {
    tracing::warn!(
        profile,
        provider = id,
        reason,
        "session profile provider was not built; turns on this profile fail instead of using the default provider"
    );
}

#[cfg(test)]
#[path = "profile_providers_tests.rs"]
mod tests;
