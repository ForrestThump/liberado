//! Split from `lib.rs` for module-health boundaries.

use super::*;
use liberado_provider::Message;

#[test]
fn constructor_sets_fields() {
    let provider = OpenAiCompatibleProvider::new("sk-abc", "my-model", "https://example.com");
    assert_eq!(provider.model(), "my-model");
    assert_eq!(provider.api_key, "sk-abc");
    assert_eq!(provider.base_url, "https://example.com");
    assert!(provider.extra_client_error_status.is_empty());
}

#[test]
fn with_overrides_sets_each_field_independently() {
    // The whole point of lifting the chain: every composition root should set model / temperature
    // / reasoning on the same code path, so adding a new override knob is a one-liner here.
    let provider = OpenAiCompatibleProvider::new("k", "default-model", "https://example.com")
        .with_overrides(
            Some("override-model".to_string()),
            Some(0.3),
            Some("high".to_string()),
        );
    assert_eq!(provider.model(), "override-model");
    assert_eq!(provider.temperature, Some(0.3));
    assert_eq!(provider.reasoning_effort.as_deref(), Some("high"));
}

#[test]
fn with_overrides_clears_fields_passed_as_none() {
    // The helper is the single source of truth for the override chain — `None` must overwrite,
    // matching how `with_temperature(None)` and `with_reasoning_effort(None)` already work.
    // Composition roots that don't want a particular override pass `None` and expect it to
    // land as the new value.
    let provider = OpenAiCompatibleProvider::new("k", "m", "https://example.com")
        .with_temperature(Some(0.7))
        .with_reasoning_effort(Some("low".into()))
        .with_overrides(None, None, None);
    assert_eq!(provider.model(), "m");
    assert_eq!(provider.temperature, None);
    assert_eq!(provider.reasoning_effort, None);
}

#[test]
fn with_overrides_leaves_model_alone_when_none() {
    // Some composition roots (e.g. `coder-runner`'s `DirectProviderFactory`) already pass the
    // model into `new` and only want reasoning out of this helper — passing `None` for the model
    // must not blank the model already configured in `new`.
    let provider = OpenAiCompatibleProvider::new("k", "configured-model", "https://example.com")
        .with_overrides(None, None, Some("high".into()));
    assert_eq!(provider.model(), "configured-model");
    assert_eq!(provider.reasoning_effort.as_deref(), Some("high"));
}

#[test]
fn with_overrides_reasoning_only_clears_a_prior_reasoning_field() {
    // Mirror of the original `with_reasoning_effort(None)` chain: clearing must work the same.
    let provider = OpenAiCompatibleProvider::new("k", "m", "https://example.com")
        .with_reasoning_effort(Some("low".into()))
        .with_overrides(None, None, None);
    assert_eq!(provider.reasoning_effort, None);
}

#[test]
fn with_overrides_ignores_a_whitespace_model() {
    // `Provider::set_model` already trims and refuses empty/whitespace — assert the helper
    // inherits that behaviour rather than bypassing it with a direct field write.
    let provider = OpenAiCompatibleProvider::new("k", "m", "https://example.com").with_overrides(
        Some("   ".to_string()),
        None,
        None,
    );
    assert_eq!(provider.model(), "m");
}

#[test]
fn with_extra_client_error_status_sets_codes() {
    let provider = OpenAiCompatibleProvider::new("k", "m", "https://example.com")
        .with_extra_client_error_status(vec![402]);
    assert_eq!(provider.extra_client_error_status, vec![402]);
}

#[test]
fn with_base_url_overrides_default() {
    let provider = OpenAiCompatibleProvider::new("k", "m", "https://a.example.com")
        .with_base_url("http://localhost:8080");
    assert_eq!(provider.base_url, "http://localhost:8080");
}

#[test]
fn endpoint_strips_trailing_slash() {
    let provider = OpenAiCompatibleProvider::new("k", "m", "https://api.example.com/");
    assert_eq!(
        provider.endpoint(),
        "https://api.example.com/chat/completions"
    );
    assert_eq!(provider.models_endpoint(), "https://api.example.com/models");
}

#[test]
fn endpoint_without_trailing_slash() {
    let provider = OpenAiCompatibleProvider::new("k", "m", "https://api.example.com");
    assert_eq!(
        provider.endpoint(),
        "https://api.example.com/chat/completions"
    );
}

#[test]
fn model_getter_returns_configured_model() {
    let provider = OpenAiCompatibleProvider::new("k", "custom-model-v2", "https://example.com");
    assert_eq!(provider.model(), "custom-model-v2");
}

#[test]
fn set_model_hot_swaps_active_id() {
    let provider = OpenAiCompatibleProvider::new("k", "deepseek-chat", "https://example.com");
    assert_eq!(provider.model(), "deepseek-chat");
    provider.set_model("deepseek-v4-pro".into());
    assert_eq!(provider.model(), "deepseek-v4-pro");
    provider.set_model("  ".into()); // empty/whitespace ignored
    assert_eq!(provider.model(), "deepseek-v4-pro");
}

#[test]
fn deepseek_from_env_uses_environment_variables() {
    let result = OpenAiCompatibleProvider::deepseek_from_env();
    if std::env::var("DEEPSEEK_API_KEY").is_ok() {
        let provider = result.expect("from_env should succeed when DEEPSEEK_API_KEY is set");
        assert_eq!(provider.api_key, std::env::var("DEEPSEEK_API_KEY").unwrap());
        assert_eq!(
            provider.base_url,
            OpenAiCompatibleProvider::DEEPSEEK_BASE_URL
        );
        assert!(provider.extra_client_error_status.is_empty());
    } else {
        assert!(
            result.is_err(),
            "from_env should fail when DEEPSEEK_API_KEY is unset"
        );
    }
}

#[test]
fn openrouter_from_env_uses_environment_variables() {
    let result = OpenAiCompatibleProvider::openrouter_from_env();
    if std::env::var("OPENROUTER_API_KEY").is_ok() {
        let provider = result.expect("from_env should succeed when OPENROUTER_API_KEY is set");
        assert_eq!(
            provider.api_key,
            std::env::var("OPENROUTER_API_KEY").unwrap()
        );
        assert_eq!(
            provider.base_url,
            OpenAiCompatibleProvider::OPENROUTER_BASE_URL
        );
        assert_eq!(provider.extra_client_error_status, vec![402]);
    } else {
        assert!(
            result.is_err(),
            "from_env should fail when OPENROUTER_API_KEY is unset"
        );
    }
}

#[test]
fn generic_from_env_works_for_an_arbitrary_new_backend() {
    // Exercise the generic constructor directly with a made-up backend/env var pair — proves
    // a brand new provider (no dedicated Rust wrapper, no `deepseek_from_env`-style helper)
    // still goes through this exact same path, which is the whole point of collapsing the two
    // old crates into this one. Doesn't mutate env vars (races under parallel test runs, same
    // reason `deepseek_from_env_uses_environment_variables` above only asserts conditionally on
    // whatever the real environment happens to be) — asserts the clean failure shape instead,
    // which is just as real a proof the generic path is wired correctly.
    let result = OpenAiCompatibleProvider::from_env(
        "LIBERADO_TEST_PROVIDER_KEY_DOES_NOT_EXIST",
        Some("LIBERADO_TEST_PROVIDER_MODEL_DOES_NOT_EXIST"),
        "some-default-model",
        "https://example.invalid",
        vec![418],
    );
    assert!(
        result.is_err(),
        "from_env should fail when its api_key_env isn't set"
    );
}

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY + network access"]
async fn deepseek_live_smoke() {
    let provider = OpenAiCompatibleProvider::deepseek_from_env().expect("DEEPSEEK_API_KEY not set");
    let resp = provider
        .complete(CompletionRequest::new(vec![Message::user(
            "Reply with exactly one word: pong",
        )]))
        .await
        .expect("live call failed");
    assert!(
        resp.content.is_some(),
        "expected text content from DeepSeek"
    );
}

#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY + network access"]
async fn openrouter_live_smoke() {
    let provider =
        OpenAiCompatibleProvider::openrouter_from_env().expect("OPENROUTER_API_KEY not set");
    let resp = provider
        .complete(CompletionRequest::new(vec![Message::user(
            "Reply with exactly one word: pong",
        )]))
        .await
        .expect("live call failed");
    assert!(
        resp.content.is_some(),
        "expected text content from OpenRouter"
    );
}

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY + network access"]
async fn deepseek_list_models_live_smoke() {
    let provider = OpenAiCompatibleProvider::deepseek_from_env().expect("DEEPSEEK_API_KEY not set");
    let models = provider.list_models().await.expect("live call failed");
    assert!(!models.is_empty(), "expected at least one model id");
}
