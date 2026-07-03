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
//!
//! **Scoring model(s) and sample count** (`docs/roadmap/heuristics-tuning-engine-plan.md`'s
//! "Real-model verification" findings): real model APIs aren't perfectly deterministic run-to-run
//! even at temperature 0, so a single sample against a single model isn't a fully trustworthy
//! signal. `scoring_models` can list several OpenRouter model slugs and `samples_per_scenario` can
//! sample each of them more than once; both default to today's cheap single-model, single-sample
//! behavior unless explicitly turned up — this is a deliberate, visible cost decision the user
//! makes, not a silent multiplier on the existing defaults.

use std::sync::Arc;

use liberado_provider::Provider;
use liberado_provider_openrouter::OpenRouterProvider;
use serde::Deserialize;

/// Plays "the real dispatcher" during scoring — defaults to OpenRouter's slug for a small, cheap
/// DeepSeek model, so a winning prompt is likely to transfer without every run being expensive.
/// Spot-checked against OpenRouter's own site as of this writing; OpenRouter can rename slugs,
/// which is exactly why this is overridable (via `tuner.toml` or `TUNER_SCORING_MODELS`) rather
/// than hardcoded deeper in the logic.
pub const DEFAULT_SCORING_MODEL: &str = "deepseek/deepseek-v4-flash";

const DEFAULT_SAMPLES_PER_SCENARIO: usize = 1;
const DEFAULT_BEAM_WIDTH: usize = 2;
const DEFAULT_COLD_STARTS_PER_GENERATION: usize = 1;
const DEFAULT_MUTATIONS_PER_CANDIDATE: usize = 2;
const DEFAULT_MAX_GENERATIONS: usize = 3;
const DEFAULT_CALL_BUDGET: usize = 350;

const TUNER_CONFIG_FILE: &str = "tuner.toml";

/// Everything a tuning session needs, resolved once at startup.
pub struct TunerConfig {
    /// One `OpenRouterProvider` per configured scoring model slug — a candidate is scored against
    /// every one of these (see `samples_per_scenario` for how many times against each).
    pub scoring_providers: Vec<Arc<dyn Provider>>,
    pub meta_provider: Arc<dyn Provider>,
    /// How many times each scenario is sampled per scoring model.
    pub samples_per_scenario: usize,
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
    scoring_models: Option<Vec<String>>,
    meta_model: Option<String>,
    samples_per_scenario: Option<usize>,
    beam_width: Option<usize>,
    cold_starts_per_generation: Option<usize>,
    mutations_per_candidate: Option<usize>,
    max_generations: Option<usize>,
    call_budget: Option<usize>,
}

impl TunerConfig {
    /// Resolve config from `tuner.toml` (if present) layered under environment variables, then
    /// build the `OpenRouterProvider`s. `OPENROUTER_API_KEY` is the only required value.
    pub fn load() -> Result<Self, ConfigError> {
        let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| ConfigError::MissingApiKey)?;
        let file = load_file_config();

        let scoring_models = resolve_model_list(
            "TUNER_SCORING_MODELS",
            file.scoring_models.clone(),
            DEFAULT_SCORING_MODEL,
        );
        let meta_model = env_string("TUNER_META_MODEL")
            .or(file.meta_model.clone())
            .unwrap_or_else(|| scoring_models[0].clone());

        let scoring_providers: Vec<Arc<dyn Provider>> = scoring_models
            .into_iter()
            .map(|model| Arc::new(OpenRouterProvider::new(api_key.clone(), model)) as Arc<dyn Provider>)
            .collect();
        let meta_provider: Arc<dyn Provider> = Arc::new(OpenRouterProvider::new(api_key, meta_model));

        Ok(Self {
            scoring_providers,
            meta_provider,
            samples_per_scenario: resolve_usize(
                "TUNER_SAMPLES_PER_SCENARIO",
                file.samples_per_scenario,
                DEFAULT_SAMPLES_PER_SCENARIO,
            ),
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

/// Resolve a list-valued tunable (scoring models): the env var (comma-separated) wins if it parses
/// to a non-empty list, else the file's list if non-empty, else a single-element list of `default`.
fn resolve_model_list(var: &str, file_value: Option<Vec<String>>, default: &str) -> Vec<String> {
    if let Ok(raw) = std::env::var(var) {
        let list: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if !list.is_empty() {
            return list;
        }
    }
    if let Some(list) = file_value {
        if !list.is_empty() {
            return list;
        }
    }
    vec![default.to_string()]
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
    fn resolve_model_list_falls_back_to_single_default() {
        assert_eq!(
            resolve_model_list("TUNER_TEST_MODELS_DOES_NOT_EXIST", None, "deepseek/default"),
            vec!["deepseek/default".to_string()]
        );
    }

    #[test]
    fn resolve_model_list_prefers_file_value_over_default() {
        let file_value = Some(vec!["a/b".to_string(), "c/d".to_string()]);
        assert_eq!(
            resolve_model_list("TUNER_TEST_MODELS_DOES_NOT_EXIST", file_value, "deepseek/default"),
            vec!["a/b".to_string(), "c/d".to_string()]
        );
    }

    #[test]
    fn file_config_with_no_fields_set_parses_as_all_none() {
        let cfg: TunerFileConfig = toml::from_str("").unwrap();
        assert!(cfg.scoring_models.is_none());
        assert!(cfg.samples_per_scenario.is_none());
        assert!(cfg.call_budget.is_none());
    }

    #[test]
    fn file_config_parses_partial_overrides_including_model_array() {
        let cfg: TunerFileConfig = toml::from_str(
            r#"
            scoring_models = ["deepseek/deepseek-v4-flash", "anthropic/claude-haiku-latest"]
            samples_per_scenario = 3
            call_budget = 999
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.scoring_models,
            Some(vec![
                "deepseek/deepseek-v4-flash".to_string(),
                "anthropic/claude-haiku-latest".to_string()
            ])
        );
        assert_eq!(cfg.samples_per_scenario, Some(3));
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
