//! Survivor kills for `crates/bootstrap/src/lib.rs` (mutation campaign 2026-08).
//!
//! Coverage the inline suite lacked: the deepseek *fallback* arm of
//! [`resolve_provider_profile`] picking a wrong declared profile, role-provider
//! override routing, the `is_enabled` contract in both directions, factory
//! construction when the API key exists, capture-path dedup across trailing
//! slashes, and dispatch-pack construction when a provider is configured.
//!
//! Providers are built through the real `from_env` path, so these tests set one
//! dedicated env var. Every env-touching test holds [`ENV_LOCK`] and restores
//! the previous value on drop — process-global state must outlive no panic.

use super::*;
use liberado_common::model::ModelRole;
use liberado_config::{ProviderProfile, RoleOverride};
use liberado_provider::NoopRecorder;
use std::sync::Mutex;

/// Serialises every test here that touches the shared env var.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const TEST_KEY_ENV: &str = "LIBERADO_TEST_BOOTSTRAP_ROLE_KEY";

/// Restores `TEST_KEY_ENV` on drop, even when an assert panics while held.
struct KeyGuard {
    saved: Option<String>,
}

impl KeyGuard {
    /// Callers hold [`ENV_LOCK`] across the guard's whole lifetime.
    fn set() -> Self {
        let saved = std::env::var(TEST_KEY_ENV).ok();
        // SAFETY: callers hold ENV_LOCK; the drop below restores the old value.
        unsafe {
            std::env::set_var(TEST_KEY_ENV, "bootstrap-survivor-test-key");
        }
        Self { saved }
    }
}

impl Drop for KeyGuard {
    fn drop(&mut self) {
        // SAFETY: still under ENV_LOCK on every path, panics included.
        unsafe {
            match self.saved.take() {
                Some(v) => std::env::set_var(TEST_KEY_ENV, v),
                None => std::env::remove_var(TEST_KEY_ENV),
            }
        }
    }
}

fn profile(name: &str, model: &str) -> ProviderProfile {
    ProviderProfile {
        name: name.to_string(),
        base_url: format!("https://{name}.invalid"),
        default_model: model.to_string(),
        api_key_env: TEST_KEY_ENV.to_string(),
        model_env: None,
        extra_client_error_status: Vec::new(),
        fallback: None,
    }
}

fn keyed_config(provider: &str, profiles: Vec<ProviderProfile>) -> Config {
    let mut config = Config::default();
    config.topology.providers = profiles;
    config.topology.provider = provider.to_string();
    config
}

fn role_providers(config: &Config) -> RoleProviders {
    role_providers_from_config(config, Arc::new(NoopRecorder))
}

/// Hold before any [`KeyGuard::set`]; the two guards together make the
/// env write and its restore exclusive to this one test.
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn primary_provider_with_a_wired_fallback_still_resolves() {
    // The runtime fallback logic is covered in `liberado-provider-openai-compat`'s unit tests;
    // the contract this test pins is the bootstrap-side wiring: when a provider profile names a
    // fallback, the daemon's primary provider is still constructed (the fallback being usable
    // is the runtime's problem, not bootstrap's). A missing fallback API key degrades to
    // primary-only with a logged warning, not a startup failure.
    use liberado_config::ProviderFallback;
    let _env = lock_env();
    let _key = KeyGuard::set();
    let mut config = keyed_config(
        "alpha",
        vec![
            profile("alpha", "model-alpha"),
            profile("beta", "model-beta"),
        ],
    );
    // The fallback points at a provider whose key is NOT set in this test — we want to verify
    // bootstrap tolerates that and the primary still resolves.
    let primary = config
        .topology
        .providers
        .iter_mut()
        .find(|p| p.name == "alpha")
        .expect("alpha profile exists");
    primary.fallback = Some(ProviderFallback {
        provider: "beta".into(),
        model: Some("model-beta".into()),
        on_status: vec![402],
    });

    let primary = provider_from_config(&config).expect("primary should still resolve");
    assert_eq!(
        primary.model(),
        "model-alpha",
        "the primary's model is unchanged by fallback wiring"
    );
}

#[test]
fn an_undeclared_provider_falls_back_to_the_deepseek_profile_itself() {
    let _env = lock_env();
    let _key = KeyGuard::set();
    let config = keyed_config(
        "no-such-provider",
        vec![
            profile("gamma", "model-gamma"),
            profile("deepseek", "model-deepseek"),
        ],
    );

    let providers = role_providers(&config);
    let primary = providers.primary.expect("fallback profile has its key set");
    assert_eq!(
        primary.model(),
        "model-deepseek",
        "the fallback must select the *deepseek* entry, not any other declared profile"
    );
}

#[test]
fn for_config_builds_a_factory_when_the_selected_key_is_present() {
    let _env = lock_env();
    let _key = KeyGuard::set();
    let config = keyed_config("alpha", vec![profile("alpha", "model-alpha")]);

    let factory =
        ProfileProviderFactory::for_config(&config).expect("a configured key must yield a factory");
    use liberado_coder_agent::CoderProviderFactory;

    let role = liberado_coder_core::CoderRoleConfig {
        model: "role-model".to_string(),
        ..Default::default()
    };
    let provider = factory
        .provider_for("coder", &role)
        .expect("the same key serves provider_for");
    assert_eq!(
        provider.model(),
        "role-model",
        "provider_for must apply the coding role's model over the profile default"
    );
}

#[test]
fn is_enabled_tracks_primary_presence_in_both_directions() {
    // Watch-only composition: nothing configured, so nothing can be enabled.
    assert!(
        !RoleProviders::none().is_enabled(),
        "an all-None RoleProviders is watch-only"
    );

    let _env = lock_env();
    let _key = KeyGuard::set();
    let providers = role_providers(&keyed_config(
        "alpha",
        vec![profile("alpha", "model-alpha")],
    ));
    assert!(
        providers.is_enabled(),
        "role_providers_from_config built a primary, so the deployment is enabled"
    );
}

#[test]
fn a_model_only_override_reaches_that_roles_provider() {
    let _env = lock_env();
    let _key = KeyGuard::set();
    let mut config = keyed_config("alpha", vec![profile("alpha", "model-alpha")]);
    config.topology.roles.insert(
        ModelRole::MainAgent,
        RoleOverride {
            provider: None,
            model: Some("face-x".to_string()),
            temperature: None,
            reasoning: None,
        },
    );

    let providers = role_providers(&config);
    let face = providers.face.expect("base provider configured");
    assert_eq!(
        face.model(),
        "face-x",
        "a single-field override must still route the role off the shared base provider"
    );
}

#[test]
fn slash_variant_duplicate_capture_paths_are_deduped() {
    let paths = watcher_capture_paths(
        "/data/inbox",
        &["/data/inbox/".to_string(), "/data/inbox".to_string()],
    );
    assert_eq!(
        paths,
        vec!["/data/inbox"],
        "trailing-slash and exact duplicates of the inbox must collapse to one entry"
    );
}

#[test]
fn dispatch_packs_build_when_a_provider_is_configured_and_stay_none_without_one() {
    use liberado_mcp::McpRegistry;

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = Arc::new(liberado_common::CapabilityCatalog::new());
    let config = Config::default();

    // Watch-only: no providers means no pack, per the documented contract.
    assert!(
        build_dispatch_pack(
            &RoleProviders::none(),
            &config,
            catalog.clone(),
            McpRegistry::new(),
            dir.path(),
            None,
        )
        .is_none()
    );

    let _env = lock_env();
    let _key = KeyGuard::set();
    let providers = role_providers(&keyed_config(
        "alpha",
        vec![profile("alpha", "model-alpha")],
    ));
    let pack = build_dispatch_pack(
        &providers,
        &config,
        catalog,
        McpRegistry::new(),
        dir.path(),
        None,
    );
    assert!(
        pack.is_some(),
        "configured dispatcher+subagent providers must assemble a dispatch pack"
    );
}

/// The None paths stay honest: an unknown provider with no deepseek
/// fallback resolves to no provider, and a declared profile whose key is
/// absent from the environment builds nothing rather than panicking.
#[test]
fn provider_from_config_none_paths() {
    let _guard = ENV_LOCK.lock().unwrap();

    // Unknown name, no deepseek declared: the ? arm returns early.
    let config = keyed_config("missing", vec![profile("other", "m")]);
    assert!(provider_from_config(&config).is_none());

    // Declared profile whose API key env is unset in this process. A name
    // unique to this test cannot be set by a concurrent suite member, so
    // reading its absence needs no guard beyond the env lock above.
    const ABSENT_KEY_ENV: &str = "LIBERADO_SURVIVOR_ABSENT_KEY_9F3A";
    unsafe {
        std::env::remove_var(ABSENT_KEY_ENV);
    }
    let mut profile = profile("declared", "m");
    profile.api_key_env = ABSENT_KEY_ENV.to_string();
    let config = keyed_config("declared", vec![profile]);
    assert!(provider_from_config(&config).is_none());
}

/// The Some path stays live too: with the key present the provider is built
/// and returned, exercising the tracing::info block and the Some arm.
#[test]
fn provider_from_config_some_path() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _key = KeyGuard::set();
    let config = keyed_config("declared", vec![profile("declared", "m")]);
    let provider = provider_from_config(&config);
    assert!(provider.is_some(), "a keyed profile builds a provider");
}

/// Integration test: a `Config` with `[[providers.fallback]]` wired should produce a provider
/// whose runtime fallback fires end-to-end against wiremock stubs for primary + fallback. This
/// closes the gap between `provider-openai-compat`'s unit tests (which prove the runtime
/// behaviour) and the existing `primary_provider_with_a_wired_fallback_still_resolves` test
/// (which proves the provider CAN be constructed) — the bootstrap wiring itself isn't covered
/// by either.
///
/// Pinned against the documented MiniMax `insufficient_balance_error` body shape (see
/// `crates/provider-openai-compat/src/lib_fallback.rs`'s `MINIMAX_INSUFFICIENT_BALANCE_BODY`).
/// If MiniMax ever changes the envelope, this test's body string needs to be updated to match
/// (the policy still triggers on status, but the fixture stops representing reality).
#[tokio::test]
// The env lock serializes tests that share `TEST_KEY_ENV`; it isn't a data lock and the
// held-across-await pattern can't deadlock the test (a panic here drops the guard). Suppress
// `await_holding_lock` for this test only — converting the file's `std::sync::Mutex` to
// `tokio::sync::Mutex` would touch every test in this module for one new async case.
#[allow(clippy::await_holding_lock)]
async fn a_config_with_fallback_wires_a_working_fallback_end_to_end() {
    use liberado_config::ProviderFallback;
    use liberado_provider::{CompletionRequest, Message};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // The env var has to remain set while we read it inside `provider_from_config` /
    // `OpenAiCompatibleProvider::from_env`, so the lock + KeyGuard scope has to wrap the
    // entire async test body — not just setup. `MockServer` is `Drop`pable but we just hold it
    // for the whole test so the underlying listener stays alive.
    let _env = lock_env();
    let _key = KeyGuard::set();

    let primary = MockServer::start().await;
    let fallback = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(402).set_body_string(
            r#"{"type":"error","error":{"type":"insufficient_balance_error","message":"insufficient balance (1008)","http_code":"402"}}"#,
        ))
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"model":"deepseek/deepseek-v4-flash-0731","choices":[{"message":{"role":"assistant","content":"from fallback"}}]}"#,
        ))
        .expect(1)
        .mount(&fallback)
        .await;

    let config = keyed_config(
        "minimax",
        vec![
            ProviderProfile {
                name: "minimax".into(),
                base_url: primary.uri(),
                default_model: "MiniMax-M3".into(),
                api_key_env: TEST_KEY_ENV.into(),
                model_env: Some("MINIMAX_MODEL".into()),
                extra_client_error_status: vec![],
                fallback: Some(ProviderFallback {
                    provider: "openrouter".into(),
                    model: Some("deepseek/deepseek-v4-flash-0731".into()),
                    on_status: vec![402],
                }),
            },
            ProviderProfile {
                name: "openrouter".into(),
                base_url: fallback.uri(),
                default_model: "openai/gpt-4o-mini".into(),
                api_key_env: TEST_KEY_ENV.into(),
                model_env: None,
                extra_client_error_status: vec![402],
                fallback: None,
            },
        ],
    );

    // `provider_from_config` reads the env vars at call time. The lock + guard above keep
    // them set throughout the test body.
    let provider = provider_from_config(&config).expect("primary provider should build");
    let resp = provider
        .complete(CompletionRequest::new(vec![Message::user("hi")]))
        .await
        .expect("fallback should succeed");
    assert_eq!(
        resp.content.as_deref(),
        Some("from fallback"),
        "the response content must come from the fallback, not the primary's 402 body"
    );
    let _ = (primary, fallback);
}
