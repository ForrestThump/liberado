//! Inference backend selection and the ACP model picker.
//!
//! Split out of `main.rs` (which had grown past 1800 lines holding the wire protocol, the session
//! store, request dispatch and this). The seam is real rather than cosmetic: nothing here knows
//! about ACP sessions or JSON-RPC — it answers "which backend, which model, what can the picker
//! show", and `main` composes it.

use std::path::PathBuf;
use std::sync::Arc;

use liberado_provider::{
    CompletionRequest, CompletionResponse, Provider, ProviderError, ProviderResult,
};
use liberado_provider_openai_compat::OpenAiCompatibleProvider;

/// Initial model shown in the picker when no API key is present.
///
/// Not a duplicate of the config default: `[[providers]]` gives openrouter `openai/gpt-4o-mini`,
/// while this is the model this bridge actually wants selected on first launch. It is a display
/// placeholder only — with a key present, the model comes from the provider profile.
pub(crate) const OPENROUTER_DEFAULT_RAW: &str = "deepseek/deepseek-v4-pro";

/// Fallback raw ids when a backend's `/models` is unreachable.
pub(crate) const OPENROUTER_FALLBACK_RAW: &[&str] =
    &["deepseek/deepseek-v4-pro", "deepseek/deepseek-v4-flash"];
pub(crate) const DEEPSEEK_FALLBACK_RAW: &[&str] = &["deepseek-chat", "deepseek-reasoner"];
pub(crate) const OPENAI_FALLBACK_RAW: &[&str] = &["gpt-4o-mini", "gpt-4o"];

#[derive(Debug, Clone)]
pub(crate) struct CatalogModel {
    pub(crate) model_id: String,
    pub(crate) name: String,
    pub(crate) description: String,
}

pub(crate) struct ResolvedProvider {
    pub(crate) provider: Arc<dyn Provider>,
    /// Backend key: `openrouter` | `deepseek` | `openai` | topology name.
    pub(crate) backend: String,
    pub(crate) model_id: String,
}

/// Config directory for ACP provider topology — same multi-tier resolution as `main.rs`.
///
/// Do **not** read `LIBERADO_CONFIG_DIR` alone: that opts out of platform dir and binary-walk
/// tiers, so an unset variable silently means no topology (the dogfood-period defect).
pub(crate) fn provider_config_dir() -> Option<PathBuf> {
    liberado_config::config_dir()
}

pub(crate) fn build_provider() -> Result<ResolvedProvider, String> {
    let model_override = std::env::var("LIBERADO_ACP_MODEL")
        .ok()
        .filter(|s| !s.is_empty());

    if let Some(config_dir) = provider_config_dir()
        && let Some(resolved) = provider_from_liberado_config(&config_dir, model_override.clone())
    {
        return Ok(resolved);
    }

    provider_from_env_profiles(model_override)
}

/// Resolve the provider from a loaded Liberado topology (`topology.toml`): the configured backend
/// plus an OpenAI-compatible endpoint if the profile is one, else the bootstrap-built provider.
/// Returns `None` when the config fails to load or declares no provider — the caller then falls
/// back to the env-key scan.
fn provider_from_liberado_config(
    config_dir: &std::path::Path,
    model_override: Option<String>,
) -> Option<ResolvedProvider> {
    match liberado_config::load_config(Some(config_dir)) {
        Ok((config, _)) => {
            let provider = liberado_bootstrap::provider_from_config(&config)?;
            let backend = config.topology.provider.clone();
            let model = model_override.clone().unwrap_or_else(|| provider.model());
            if let Some(profile) = config
                .topology
                .providers
                .iter()
                .find(|p| p.name == config.topology.provider)
                && let Ok(p) = OpenAiCompatibleProvider::from_env(
                    &profile.api_key_env,
                    profile.model_env.as_deref(),
                    &model,
                    &profile.base_url,
                    profile.extra_client_error_status.clone(),
                )
            {
                tracing::info!(
                    provider = %profile.name,
                    %model,
                    config_dir = %config_dir.display(),
                    "acp provider from resolved Liberado config"
                );
                return Some(ResolvedProvider {
                    provider: Arc::new(p),
                    backend,
                    model_id: model,
                });
            }
            let m = provider.model();
            Some(ResolvedProvider {
                provider,
                backend,
                model_id: m,
            })
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                config_dir = %config_dir.display(),
                "resolved Liberado config load failed; falling back to env keys"
            );
            None
        }
    }
}

/// Fallback scan of declared provider profiles against the environment, preferring OpenRouter so
/// the picker gets `author/model` ids (deepseek/deepseek-v4-pro, …), then whatever else is
/// declared. Ends with a keyless placeholder provider so Paseo can still detect liberado-acp even
/// though prompts need a key.
///
/// The profiles come from `Topology::default()`, not a list written out here. This block used to
/// restate the base URLs, key envs, default models and OpenRouter's `402` "insufficient credits"
/// status — all of which `config-loader`'s `default_providers()` already declares. Two
/// declarations of one fact is failure-mode class 6: nothing compares them, so they drift and the
/// drift is silent. Adding a backend is now an entry in `[[providers]]`, which is where the
/// config model already says it belongs.
fn provider_from_env_profiles(model_override: Option<String>) -> Result<ResolvedProvider, String> {
    let profiles = liberado_config::Topology::default().providers;
    let preferred = ["openrouter", "deepseek"];
    let ordered = preferred
        .iter()
        .filter_map(|want| profiles.iter().find(|p| p.name == *want))
        .chain(
            profiles
                .iter()
                .filter(|p| !preferred.contains(&p.name.as_str())),
        );

    for profile in ordered {
        if std::env::var_os(&profile.api_key_env).is_none() {
            continue;
        }
        let model = model_override
            .clone()
            .unwrap_or_else(|| profile.default_model.clone());
        let p = OpenAiCompatibleProvider::from_env(
            &profile.api_key_env,
            profile.model_env.as_deref(),
            &model,
            &profile.base_url,
            profile.extra_client_error_status.clone(),
        )
        .map_err(|e| format!("provider init ({}): {e}", profile.api_key_env))?;
        tracing::info!(
            key_env = %profile.api_key_env,
            %model,
            base = %profile.base_url,
            backend = %profile.name,
            "acp provider ready"
        );
        return Ok(ResolvedProvider {
            provider: Arc::new(p),
            backend: profile.name.clone(),
            model_id: model,
        });
    }

    let model = model_override.unwrap_or_else(|| OPENROUTER_DEFAULT_RAW.to_string());
    tracing::warn!(
        "no API key found for any declared provider (see `[[providers]]` in topology.toml);          Paseo can still detect liberado-acp, but prompts need a key"
    );
    Ok(ResolvedProvider {
        provider: Arc::new(MissingKeyProvider {
            model: std::sync::RwLock::new(model.clone()),
        }),
        backend: "none".into(),
        model_id: model,
    })
}

/// Build the ACP model picker from the live provider catalog.
pub(crate) async fn load_model_catalog(
    provider: &dyn Provider,
    backend: &str,
    current: &str,
) -> Vec<CatalogModel> {
    let live = fetch_live_models(provider, backend).await;

    let ordered = if live.is_empty() {
        fallback_model_ids(backend, current)
    } else {
        catalog_model_ids(&live, current)
    };

    ordered
        .into_iter()
        .map(|id| CatalogModel {
            name: display_name_for(&id),
            description: description_for(backend, &id),
            model_id: id.clone(),
        })
        .collect()
}

/// Fetch the live `/models` catalog. Logs the outcome; an empty result (failure or empty list)
/// means the caller falls back to the static lists.
async fn fetch_live_models(provider: &dyn Provider, backend: &str) -> Vec<String> {
    match provider.list_models().await {
        Ok(ids) if !ids.is_empty() => {
            tracing::info!(count = ids.len(), %backend, "fetched live /models catalog");
            ids
        }
        Ok(_) => {
            tracing::warn!(%backend, "provider /models returned empty; using fallbacks");
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, %backend, "provider list_models failed; using fallbacks");
            Vec::new()
        }
    }
}

/// Full live catalog, A–Z. Includes `current` if the live list omitted it (e.g. custom slug).
pub(crate) fn catalog_model_ids(live: &[String], current: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in live {
        if !out.iter().any(|x| x == id) {
            out.push(id.clone());
        }
    }
    if !current.is_empty() && !out.iter().any(|x| x == current) {
        out.push(current.to_string());
    }
    out.sort();
    out
}

pub(crate) fn fallback_model_ids(backend: &str, current: &str) -> Vec<String> {
    let mut out: Vec<String> = match backend {
        "openrouter" => OPENROUTER_FALLBACK_RAW
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        "deepseek" => DEEPSEEK_FALLBACK_RAW
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        "openai" => OPENAI_FALLBACK_RAW
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        _ => OPENROUTER_FALLBACK_RAW
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
    };
    if !current.is_empty() && !out.iter().any(|x| x == current) {
        out.push(current.to_string());
    }
    out.sort();
    out
}

pub(crate) fn display_name_for(model_id: &str) -> String {
    // Keep the full author/model slug visible — that is the identity Paseo should show.
    model_id.to_string()
}

pub(crate) fn description_for(backend: &str, model_id: &str) -> String {
    match backend {
        "openrouter" => format!("OpenRouter · {model_id}"),
        "deepseek" => format!("DeepSeek API · {model_id}"),
        "openai" => format!("OpenAI · {model_id}"),
        other => format!("{other} · {model_id}"),
    }
}

pub(crate) struct MissingKeyProvider {
    model: std::sync::RwLock<String>,
}

#[async_trait::async_trait]
impl Provider for MissingKeyProvider {
    fn model(&self) -> String {
        self.model.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn set_model(&self, model: String) {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        *self.model.write().unwrap_or_else(|e| e.into_inner()) = model.to_string();
    }
    async fn list_models(&self) -> ProviderResult<Vec<String>> {
        Ok(OPENROUTER_FALLBACK_RAW
            .iter()
            .map(|s| (*s).to_string())
            .collect())
    }
    async fn complete(&self, _request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        Err(ProviderError::InvalidRequest(
            "liberado-acp: no API key configured. Set OPENROUTER_API_KEY (preferred), \
             DEEPSEEK_API_KEY, or OPENAI_API_KEY — or place a Liberado config directory where \
             liberado_config::config_dir resolves (LIBERADO_CONFIG_DIR is only the first tier)."
                .into(),
        ))
    }
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
