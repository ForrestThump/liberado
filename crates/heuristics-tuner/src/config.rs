//! Per-session tuning configuration. Every tunable except the API key can come from three layers,
//! lowest to highest precedence — **code default → `tuner.toml` → environment variable** — the
//! same layering `liberado-config`'s topology/policy/tuning files already use (Decision 14), so a
//! file holds the usual per-session values and an env var is a one-off override on top without
//! editing the file. `OPENROUTER_API_KEY` is the one exception: it's a secret, never read from a
//! file (Decision 10 — secrets are not config).
//!
//! `tuner.toml` lives in the same directory `liberado-config`'s `config_dir()` already resolves
//! (`config.example/tuner.toml` is the template) — nothing here searches a second location, so a
//! deployment that's already configured the daemon has one config directory to know about, not
//! two. The call budget and generation count are deliberately a human's per-session decision, not
//! a fixed global default (`docs/roadmap/heuristics-tuning-engine-plan.md`) — the values here are
//! cheap-to-run starting points, not a target to converge on.

use std::sync::Arc;

use liberado_provider::Provider;
use liberado_provider_openrouter::OpenRouterProvider;
use serde::Deserialize;

/// Plays "the real dispatcher" during scoring — defaults to OpenRouter's slug for a small, cheap
/// DeepSeek model, so a winning prompt is likely to transfer without every run being expensive.
/// Spot-checked against OpenRouter's own site as of this writing; OpenRouter can rename slugs,
/// which is exactly why this is overridable (via `tuner.toml` or `TUNER_SCORING_MODEL`) rather
/// than hardcoded deeper in the logic.
pub const DEFAULT_SCORING_MODEL: &str = "deepseek/deepseek-v4-flash";

const DEFAULT_BEAM_WIDTH: usize = 2;
const DEFAULT_COLD_STARTS_PER_GENERATION: usize = 1;
const DEFAULT_MUTATIONS_PER_CANDIDATE: usize = 2;
const DEFAULT_MAX_GENERATIONS: usize = 3;
const DEFAULT_CALL_BUDGET: usize = 350;

const TUNER_CONFIG_FILE: &str = "tuner.toml";

/// Everything a tuning session needs, resolved once at startup.
pub struct TunerConfig {
    pub scoring_provider: Arc<dyn Provider>,
    pub meta_provider: Arc<dyn Provider>,
    pub beam_width: usize,
    pub cold_starts_per_generation: usize,
    pub mutations_per_candidate: usize,
    pub max_generations: usize,
    pub call_budget: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("OPENROUTER_API_KEY is not set")]
    MissingApiKey,
}

/// `tuner.toml`'s shape — every field optional, so a partial file (or no file at all) leaves the
/// rest at their code defaults, same convention as `topology.toml`/`policy.toml`/`tuning.toml`.
#[derive(Debug, Default, Deserialize)]
struct TunerFileConfig {
    scoring_model: Option<String>,
    meta_model: Option<String>,
    beam_width: Option<usize>,
    cold_starts_per_generation: Option<usize>,
    mutations_per_candidate: Option<usize>,
    max_generations: Option<usize>,
    call_budget: Option<usize>,
}

impl TunerConfig {
    /// Resolve config from `tuner.toml` (if present) layered under environment variables, then
    /// build the two `OpenRouterProvider`s. `OPENROUTER_API_KEY` is the only required value.
    pub fn load() -> Result<Self, ConfigError> {
        let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| ConfigError::MissingApiKey)?;
        let file = load_file_config();

        let scoring_model = env_string("TUNER_SCORING_MODEL")
            .or(file.scoring_model.clone())
            .unwrap_or_else(|| DEFAULT_SCORING_MODEL.to_string());
        let meta_model = env_string("TUNER_META_MODEL")
            .or(file.meta_model.clone())
            .unwrap_or_else(|| scoring_model.clone());

        let scoring_provider: Arc<dyn Provider> =
            Arc::new(OpenRouterProvider::new(api_key.clone(), scoring_model));
        let meta_provider: Arc<dyn Provider> = Arc::new(OpenRouterProvider::new(api_key, meta_model));

        Ok(Self {
            scoring_provider,
            meta_provider,
            beam_width: resolve_usize("TUNER_BEAM_WIDTH", file.beam_width, DEFAULT_BEAM_WIDTH),
            cold_starts_per_generation: resolve_usize(
                "TUNER_COLD_STARTS_PER_GENERATION",
                file.cold_starts_per_generation,
                DEFAULT_COLD_STARTS_PER_GENERATION,
            ),
            mutations_per_candidate: resolve_usize(
                "TUNER_MUTATIONS_PER_CANDIDATE",
                file.mutations_per_candidate,
                DEFAULT_MUTATIONS_PER_CANDIDATE,
            ),
            max_generations: resolve_usize(
                "TUNER_MAX_GENERATIONS",
                file.max_generations,
                DEFAULT_MAX_GENERATIONS,
            ),
            call_budget: resolve_usize("TUNER_CALL_BUDGET", file.call_budget, DEFAULT_CALL_BUDGET),
        })
    }
}

/// Read and parse `tuner.toml` from `liberado_config::config_dir()`, if both the directory and the
/// file resolve. Absent file, absent config dir, or a parse error are all treated the same way —
/// fall back to defaults/env rather than fail the whole session over an optional file (a parse
/// error is logged, not silent).
fn load_file_config() -> TunerFileConfig {
    let Some(dir) = liberado_config::config_dir() else {
        return TunerFileConfig::default();
    };
    let path = dir.join(TUNER_CONFIG_FILE);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return TunerFileConfig::default();
    };
    match toml::from_str(&content) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "tuner.toml failed to parse — ignoring, using defaults/env");
            TunerFileConfig::default()
        }
    }
}

fn env_string(var: &str) -> Option<String> {
    std::env::var(var).ok()
}

/// Resolve one `usize` tunable: env var wins if set, else the file's value, else `default`.
fn resolve_usize(var: &str, file_value: Option<usize>, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .or(file_value)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_usize_falls_back_to_default_when_nothing_set() {
        assert_eq!(resolve_usize("TUNER_TEST_VAR_DOES_NOT_EXIST", None, 7), 7);
    }

    #[test]
    fn resolve_usize_prefers_file_value_over_default() {
        assert_eq!(resolve_usize("TUNER_TEST_VAR_DOES_NOT_EXIST", Some(9), 7), 9);
    }

    #[test]
    fn file_config_with_no_fields_set_parses_as_all_none() {
        let cfg: TunerFileConfig = toml::from_str("").unwrap();
        assert!(cfg.scoring_model.is_none());
        assert!(cfg.call_budget.is_none());
    }

    #[test]
    fn file_config_parses_partial_overrides() {
        let cfg: TunerFileConfig = toml::from_str(
            r#"
            scoring_model = "some/model"
            call_budget = 999
            "#,
        )
        .unwrap();
        assert_eq!(cfg.scoring_model.as_deref(), Some("some/model"));
        assert_eq!(cfg.call_budget, Some(999));
        assert!(cfg.beam_width.is_none());
    }

    #[test]
    fn load_fails_without_api_key() {
        // Mirrors provider-openrouter's own from_env test: reads real env state rather than
        // mutating it, so this only asserts in the (expected, in CI) case the key is unset.
        if std::env::var("OPENROUTER_API_KEY").is_err() {
            assert!(matches!(TunerConfig::load(), Err(ConfigError::MissingApiKey)));
        }
    }
}
