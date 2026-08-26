//! Coverage for `TunerConfig::load` (mutation campaign 2026-08): the env-key
//! requirement and the default model resolution, without touching the process
//! tuner.toml discovery (the fixture sets only variables nothing else reads).

use super::*;

/// Restores `OPENROUTER_API_KEY` on drop; callers hold [`ENV_LOCK`].
struct KeyGuard {
    saved: Option<String>,
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl KeyGuard {
    fn set(value: Option<&str>) -> Self {
        let saved = std::env::var("OPENROUTER_API_KEY").ok();
        // SAFETY: callers hold ENV_LOCK across the guard's lifetime.
        unsafe {
            match value {
                Some(v) => std::env::set_var("OPENROUTER_API_KEY", v),
                None => std::env::remove_var("OPENROUTER_API_KEY"),
            }
        }
        Self { saved }
    }
}

impl Drop for KeyGuard {
    fn drop(&mut self) {
        // SAFETY: still under ENV_LOCK on every path.
        unsafe {
            match self.saved.take() {
                Some(v) => std::env::set_var("OPENROUTER_API_KEY", v),
                None => std::env::remove_var("OPENROUTER_API_KEY"),
            }
        }
    }
}

#[test]
fn load_fails_closed_without_an_api_key() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _key = KeyGuard::set(None);
    assert!(
        TunerConfig::load().is_err(),
        "no OPENROUTER_API_KEY must be a MissingApiKey error"
    );
}

#[test]
fn load_builds_providers_and_defaults_with_only_a_key_set() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _key = KeyGuard::set(Some("tuner-test-key"));

    let config = TunerConfig::load().expect("a key is the only requirement");
    assert_eq!(
        config.scoring_providers.len(),
        1,
        "no TUNER_SCORING_MODELS means exactly the one default scoring model"
    );
}
