//! Which configured model id is sent to which provider.
//!
//! `list_models` is a live HTTP call, and load does not read API keys, so a catalog is
//! not available here. The offline rule is the shape of the id. An id that contains `/`
//! (`deepseek/deepseek-v4-flash`) is a vendor/model id. A provider is treated as able to
//! accept that shape when its own `default_model` also contains `/` (OpenRouter ships
//! `openai/gpt-4o-mini`; MiniMax and DeepSeek ship bare slugs). A bare id is not checked:
//! any configured provider might accept it, and the turn error names it if the backend
//! rejects it.
//!
//! The first segment is not a route. `deepseek/deepseek-v4-flash` is an OpenRouter slug.
//! The default table also has a provider named `deepseek`, and that API wants
//! `deepseek-chat`. Routing on the prefix would send a valid OpenRouter slug to the
//! direct API whenever both are configured.

use liberado_common::{Error, Result};

use super::config::Config;
use super::topology::ProviderProfile;

impl Config {
    pub(super) fn validate_model_routes(&self) -> Result<()> {
        for profile in &self.topology.session_profiles {
            if profile.enabled {
                check_model(
                    self,
                    &format!("topology.session_profiles['{}'].model", profile.name),
                    profile.model.as_deref(),
                    profile.provider.as_deref(),
                )?;
            }
        }
        let mut roles: Vec<_> = self.topology.roles.iter().collect();
        roles.sort_by_key(|(role, _)| role.as_str());
        for (role, override_) in roles {
            check_model(
                self,
                &format!("topology.roles.{}.model", role.as_str()),
                override_.model.as_deref(),
                override_.provider.as_deref(),
            )?;
        }
        for provider in &self.topology.providers {
            if let Some(fallback) = &provider.fallback {
                check_model(
                    self,
                    &format!("topology.providers['{}'].fallback.model", provider.name),
                    fallback.model.as_deref(),
                    Some(fallback.provider.as_str()),
                )?;
            }
        }
        Ok(())
    }
}

fn check_model(
    config: &Config,
    label: &str,
    model: Option<&str>,
    explicit_provider: Option<&str>,
) -> Result<()> {
    match trimmed(model) {
        Id::Absent => Ok(()),
        Id::Blank => Err(Error::Config(format!(
            "{label} is empty — omit the key to use the provider default, or name a model id"
        ))),
        Id::Set(model) => check_set(config, label, model, explicit_provider),
    }
}

fn check_set(
    config: &Config,
    label: &str,
    model: &str,
    explicit_provider: Option<&str>,
) -> Result<()> {
    let provider_id = explicit_provider
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(config.topology.provider.as_str());
    let census = provider_census(config);
    let Some(provider) = find_provider(config, provider_id) else {
        return Err(Error::Config(format!(
            "{label} '{model}' names provider '{provider_id}', which is not a \
             topology.providers entry. Providers considered: {census}"
        )));
    };
    if model_fits(provider, model) {
        return Ok(());
    }
    Err(Error::Config(format!(
        "{label} '{model}' would be sent to provider '{}', whose default model '{}' \
         does not use a vendor/model id. Startup does not call list_models, so an id \
         containing '/' is accepted only when the receiving provider's default_model \
         also contains '/'.{} Providers considered: {census}",
        provider.name,
        provider.default_model,
        prefix_note(config, model, &provider.name)
    )))
}

/// A bare slug is unchecked. A vendor/model id fits a provider that already uses one.
fn model_fits(provider: &ProviderProfile, model: &str) -> bool {
    !model.contains('/') || provider.default_model.contains('/')
}

fn prefix_note(config: &Config, model: &str, selected: &str) -> String {
    let Some(prefix) = model.split_once('/').map(|(prefix, _)| prefix) else {
        return String::new();
    };
    if prefix.is_empty() {
        return String::new();
    }
    let Some(named) = find_provider(config, prefix) else {
        return String::new();
    };
    if named.name == selected {
        return format!(
            " The id prefix '{prefix}' matches the selected provider, but its default \
             model '{}' is not a vendor/model id.",
            named.default_model
        );
    }
    format!(
        " The id prefix '{prefix}' matches provider '{}', which is not selected. \
         Prefixes are not used as routes.",
        named.name
    )
}

fn provider_census(config: &Config) -> String {
    if config.topology.providers.is_empty() {
        return "none".to_string();
    }
    config
        .topology
        .providers
        .iter()
        .map(|provider| {
            let shape = if provider.default_model.contains('/') {
                "accepts vendor/model ids"
            } else {
                "direct ids only"
            };
            format!(
                "{} (default model '{}', {shape})",
                provider.name, provider.default_model
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn find_provider<'a>(config: &'a Config, id: &str) -> Option<&'a ProviderProfile> {
    config
        .topology
        .providers
        .iter()
        .find(|provider| provider.name == id)
}

enum Id<'a> {
    Absent,
    Blank,
    Set(&'a str),
}

fn trimmed(model: Option<&str>) -> Id<'_> {
    match model.map(str::trim) {
        None => Id::Absent,
        Some("") => Id::Blank,
        Some(model) => Id::Set(model),
    }
}

#[cfg(test)]
#[path = "config_model_route_tests.rs"]
mod tests;
