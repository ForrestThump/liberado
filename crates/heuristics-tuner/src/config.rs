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
//! a fixed global default (`docs/future-work/heuristics-tuning-engine-plan.md`) — the values here are
//! cheap-to-run starting points, not a target to converge on.
//!
//! **Scoring model(s) and sample count** (`docs/future-work/heuristics-tuning-engine-plan.md`'s
//! "Real-model verification" findings): real model APIs aren't perfectly deterministic run-to-run
//! even at temperature 0, so a single sample against a single model isn't a fully trustworthy
//! signal. `scoring_models` can list several OpenRouter model slugs and `samples_per_scenario` can
//! sample each of them more than once; both default to today's cheap single-model, single-sample
//! behavior unless explicitly turned up — this is a deliberate, visible cost decision the user
//! makes, not a silent multiplier on the existing defaults. `max_scenarios` limits scoring to the
//! first N of `liberado_eval::scenarios()` (declaration order) — unset (`None`) by default, meaning
//! every scenario — for cheaply smoke-testing a session's plumbing before committing to a full,
//! comprehensive run.

use std::sync::Arc;

use liberado_provider::Provider;
use liberado_provider_openai_compat::OpenAiCompatibleProvider;
use serde::Deserialize;

use crate::coder_scenarios::CoderTier;

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

/// Which role's system prompt this session tunes.
/// `Dispatcher` is the default; `Executor`/`Subagent` tune tool-loop preambles; `Coder` tunes the
/// Liberado coding-worker system prompt against real workspace scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layer {
    #[default]
    Dispatcher,
    Executor,
    Subagent,
    /// Liberado `coder-agent` / PR-dispatch coding worker prompt.
    Coder,
}

/// Everything a tuning session needs, resolved once at startup.
pub struct TunerConfig {
    pub layer: Layer,
    /// One OpenRouter-backed `OpenAiCompatibleProvider` per configured scoring model slug — a
    /// candidate is scored against every one of these (see `samples_per_scenario` for how many
    /// times against each).
    pub scoring_providers: Vec<Arc<dyn Provider>>,
    pub meta_provider: Arc<dyn Provider>,
    /// How many times each scenario is sampled per scoring model.
    pub samples_per_scenario: usize,
    /// Score only the first `max_scenarios` of the active scenario list (declaration order),
    /// or all of them when `None`. For cheaply smoke-testing the pipeline before a comprehensive
    /// run — not meant to be a representative subset, just a quick, reproducible slice.
    pub max_scenarios: Option<usize>,
    /// Progressive coding curriculum tier (smoke ⊂ core ⊂ stress ⊂ greenfield).
    pub coder_tier: CoderTier,
    /// Optional allowlist of coder scenario names (`TUNER_CODER_SCENARIOS=a,b,c`).
    pub coder_scenario_filter: Option<Vec<String>>,
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
    layer: Option<String>,
    scoring_models: Option<Vec<String>>,
    meta_model: Option<String>,
    samples_per_scenario: Option<usize>,
    max_scenarios: Option<usize>,
    /// `smoke` | `core` | `stress` | `greenfield` — coder curriculum only.
    coder_tier: Option<String>,
    /// Comma-separated scenario names when set via file (usually use env instead).
    coder_scenarios: Option<String>,
    beam_width: Option<usize>,
    cold_starts_per_generation: Option<usize>,
    mutations_per_candidate: Option<usize>,
    max_generations: Option<usize>,
    call_budget: Option<usize>,
}

impl TunerConfig {
    /// Resolve config from `tuner.toml` (if present) layered under environment variables, then
    /// build the OpenRouter-backed `OpenAiCompatibleProvider`s. `OPENROUTER_API_KEY` is the only
    /// required value.
    pub fn load() -> Result<Self, ConfigError> {
        let api_key =
            std::env::var("OPENROUTER_API_KEY").map_err(|_| ConfigError::MissingApiKey)?;
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
            .map(|model| {
                Arc::new(OpenAiCompatibleProvider::new(
                    api_key.clone(),
                    model,
                    OpenAiCompatibleProvider::OPENROUTER_BASE_URL,
                )) as Arc<dyn Provider>
            })
            .collect();
        let meta_provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::new(
            api_key,
            meta_model,
            OpenAiCompatibleProvider::OPENROUTER_BASE_URL,
        ));

        Ok(Self {
            layer: resolve_layer(file.layer.clone()),
            scoring_providers,
            meta_provider,
            samples_per_scenario: resolve_usize(
                "TUNER_SAMPLES_PER_SCENARIO",
                file.samples_per_scenario,
                DEFAULT_SAMPLES_PER_SCENARIO,
            ),
            max_scenarios: resolve_optional_usize("TUNER_MAX_SCENARIOS", file.max_scenarios),
            coder_tier: resolve_coder_tier(file.coder_tier.clone()),
            coder_scenario_filter: resolve_scenario_filter(file.coder_scenarios.clone()),
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

/// Resolve an optional `usize` tunable that has no numeric default (`None` — "no limit" — is itself
/// the valid default): env var wins if it parses, else the file's value, else `None`.
fn resolve_optional_usize(var: &str, file_value: Option<usize>) -> Option<usize> {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .or(file_value)
}

/// Resolve which layer this session tunes: env var wins if set, else the file's value, else
/// `Layer::default()` (`Dispatcher`). An unrecognized value falls back to the default with a
/// warning rather than failing the session — same posture as a `tuner.toml` parse error.
fn resolve_layer(file_value: Option<String>) -> Layer {
    let raw = env_string("TUNER_LAYER").or(file_value);
    match raw.as_deref().map(str::to_lowercase).as_deref() {
        Some("dispatcher") | None => Layer::Dispatcher,
        Some("executor") => Layer::Executor,
        Some("subagent") => Layer::Subagent,
        Some("coder") | Some("coding") => Layer::Coder,
        Some(other) => {
            tracing::warn!(value = %other, "unknown tuner layer — defaulting to dispatcher");
            Layer::Dispatcher
        }
    }
}

/// Default **core** so first serious runs include multi-file + safety without full stress cost.
fn resolve_coder_tier(file_value: Option<String>) -> CoderTier {
    let raw = env_string("TUNER_CODER_TIER").or(file_value);
    match raw.as_deref() {
        None => CoderTier::Core,
        Some(s) => match CoderTier::parse(s) {
            Some(t) => t,
            None => {
                tracing::warn!(value = %s, "unknown TUNER_CODER_TIER — defaulting to core");
                CoderTier::Core
            }
        },
    }
}

fn resolve_scenario_filter(file_value: Option<String>) -> Option<Vec<String>> {
    let raw = env_string("TUNER_CODER_SCENARIOS").or(file_value)?;
    let list: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if list.is_empty() { None } else { Some(list) }
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
    if let Some(list) = file_value
        && !list.is_empty()
    {
        return list;
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
        assert_eq!(
            resolve_usize("TUNER_TEST_VAR_DOES_NOT_EXIST", Some(9), 7),
            9
        );
    }

    #[test]
    fn resolve_optional_usize_falls_back_to_none_when_nothing_set() {
        assert_eq!(
            resolve_optional_usize("TUNER_TEST_VAR_DOES_NOT_EXIST", None),
            None
        );
    }

    #[test]
    fn resolve_optional_usize_prefers_file_value_when_set() {
        assert_eq!(
            resolve_optional_usize("TUNER_TEST_VAR_DOES_NOT_EXIST", Some(3)),
            Some(3)
        );
    }

    #[test]
    fn resolve_layer_defaults_to_dispatcher_when_nothing_set() {
        assert_eq!(resolve_layer(None), Layer::Dispatcher);
    }

    #[test]
    fn resolve_layer_prefers_file_value() {
        assert_eq!(resolve_layer(Some("executor".to_string())), Layer::Executor);
    }

    #[test]
    fn resolve_layer_is_case_insensitive() {
        assert_eq!(resolve_layer(Some("Executor".to_string())), Layer::Executor);
    }

    #[test]
    fn resolve_layer_recognizes_subagent() {
        assert_eq!(resolve_layer(Some("subagent".to_string())), Layer::Subagent);
    }

    #[test]
    fn resolve_layer_recognizes_coder() {
        assert_eq!(resolve_layer(Some("coder".to_string())), Layer::Coder);
        assert_eq!(resolve_layer(Some("coding".to_string())), Layer::Coder);
    }

    #[test]
    fn resolve_coder_tier_defaults_to_core() {
        assert_eq!(resolve_coder_tier(None), CoderTier::Core);
        assert_eq!(
            resolve_coder_tier(Some("stress".to_string())),
            CoderTier::Stress
        );
        assert_eq!(
            resolve_coder_tier(Some("nope".to_string())),
            CoderTier::Core
        );
    }

    #[test]
    fn resolve_layer_falls_back_to_dispatcher_on_unrecognized_value() {
        assert_eq!(
            resolve_layer(Some("subagent-typo".to_string())),
            Layer::Dispatcher
        );
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
            resolve_model_list(
                "TUNER_TEST_MODELS_DOES_NOT_EXIST",
                file_value,
                "deepseek/default"
            ),
            vec!["a/b".to_string(), "c/d".to_string()]
        );
    }

    #[test]
    fn file_config_with_no_fields_set_parses_as_all_none() {
        let cfg: TunerFileConfig = toml::from_str("").unwrap();
        assert!(cfg.layer.is_none());
        assert!(cfg.scoring_models.is_none());
        assert!(cfg.samples_per_scenario.is_none());
        assert!(cfg.max_scenarios.is_none());
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
}

#[cfg(test)]
#[path = "config_load_tests.rs"]
mod load_tests;
