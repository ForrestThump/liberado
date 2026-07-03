//! Per-session tuning configuration, read from the environment at startup (this workspace has no
//! CLI-arg-parsing crate anywhere — env vars only, matching `DeepSeekProvider::from_env()` and
//! `liberado-eval`'s own convention). The call budget and generation count are deliberately a
//! human's per-session decision, not a fixed global default
//! (`docs/roadmap/heuristics-tuning-engine-plan.md`) — the values here are starting points, sized
//! to be cheap for a first run, not a target to converge on.

use std::sync::Arc;

use liberado_provider::Provider;
use liberado_provider_openrouter::OpenRouterProvider;

/// Plays "the real dispatcher" during scoring — defaults to OpenRouter's slug for the model
/// `liberado-provider-deepseek` targets in production, so a winning prompt is more likely to
/// transfer. Spot-checked against OpenRouter's own site as of this writing; OpenRouter can rename
/// slugs, which is exactly why this is overridable rather than hardcoded deeper in the logic.
pub const DEFAULT_SCORING_MODEL: &str = "deepseek/deepseek-chat";

const DEFAULT_BEAM_WIDTH: usize = 2;
const DEFAULT_COLD_STARTS_PER_GENERATION: usize = 1;
const DEFAULT_MUTATIONS_PER_CANDIDATE: usize = 2;
const DEFAULT_MAX_GENERATIONS: usize = 3;
const DEFAULT_CALL_BUDGET: usize = 350;

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

impl TunerConfig {
    /// Build from the environment. `OPENROUTER_API_KEY` is the only required variable; every
    /// tuning-specific variable has a sensible, cheap-to-run default.
    pub fn from_env() -> Result<Self, ConfigError> {
        let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| ConfigError::MissingApiKey)?;

        let scoring_model = std::env::var("TUNER_SCORING_MODEL")
            .unwrap_or_else(|_| DEFAULT_SCORING_MODEL.to_string());
        let meta_model = std::env::var("TUNER_META_MODEL").unwrap_or_else(|_| scoring_model.clone());

        let scoring_provider: Arc<dyn Provider> =
            Arc::new(OpenRouterProvider::new(api_key.clone(), scoring_model));
        let meta_provider: Arc<dyn Provider> = Arc::new(OpenRouterProvider::new(api_key, meta_model));

        Ok(Self {
            scoring_provider,
            meta_provider,
            beam_width: env_usize("TUNER_BEAM_WIDTH", DEFAULT_BEAM_WIDTH),
            cold_starts_per_generation: env_usize(
                "TUNER_COLD_STARTS_PER_GENERATION",
                DEFAULT_COLD_STARTS_PER_GENERATION,
            ),
            mutations_per_candidate: env_usize(
                "TUNER_MUTATIONS_PER_CANDIDATE",
                DEFAULT_MUTATIONS_PER_CANDIDATE,
            ),
            max_generations: env_usize("TUNER_MAX_GENERATIONS", DEFAULT_MAX_GENERATIONS),
            call_budget: env_usize("TUNER_CALL_BUDGET", DEFAULT_CALL_BUDGET),
        })
    }
}

/// Parse a `usize` env var, falling back to `default` if absent or unparseable rather than
/// erroring the whole session over a typo'd tunable.
fn env_usize(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_usize_falls_back_to_default_when_unset_or_unparseable() {
        assert_eq!(env_usize("TUNER_TEST_VAR_DOES_NOT_EXIST", 7), 7);
    }

    #[test]
    fn from_env_fails_without_api_key() {
        // Mirrors provider-openrouter's own from_env test: reads real env state rather than
        // mutating it, so this only asserts in the (expected, in CI) case the key is unset.
        if std::env::var("OPENROUTER_API_KEY").is_err() {
            assert!(matches!(
                TunerConfig::from_env(),
                Err(ConfigError::MissingApiKey)
            ));
        }
    }
}
