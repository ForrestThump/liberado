//! Split from `provider.rs` for module-health boundaries.

use super::*;

/// Production must call the shared resolver, not a bare env read.
///
/// Restoring `std::env::var_os("LIBERADO_CONFIG_DIR")` in `build_provider` compiles and
/// reintroduces silent empty-config when the variable is unset — the dogfood failure mode.
#[test]
fn build_provider_does_not_read_config_dir_env_directly() {
    let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/provider.rs"));
    let production = src.split("#[cfg(test)]").next().expect("production half");
    // Allow the error string / comments to mention the env name; ban the call site.
    let code_only: String = production
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let build_provider = code_only
        .split_once("pub(crate) fn build_provider()")
        .and_then(|(_, tail)| tail.split_once("pub(crate) async fn load_model_catalog"))
        .map(|(body, _)| body)
        .expect("provider source must contain the build_provider body");
    assert!(
        !build_provider.contains("var_os(\"LIBERADO_CONFIG_DIR\")")
            && !build_provider.contains("var(\"LIBERADO_CONFIG_DIR\")"),
        "build_provider must not read LIBERADO_CONFIG_DIR via env; use provider_config_dir()"
    );
    assert!(
        build_provider.contains("provider_config_dir()"),
        "build_provider must call provider_config_dir()"
    );
    assert!(
        code_only.contains("liberado_config::config_dir()"),
        "provider_config_dir must be liberado_config::config_dir"
    );
}

#[test]
fn provider_config_dir_is_the_shared_resolver() {
    // Same function the rest of the bridge uses — identity of resolution, not of result
    // (env races under parallel tests).
    let a = provider_config_dir();
    let b = liberado_config::config_dir();
    assert_eq!(
        a, b,
        "ACP provider config dir must match liberado_config::config_dir()"
    );
}

#[test]
fn catalog_ids_dedupe_sort_and_keep_the_current_pick() {
    let live = vec![
        "zeta/model".to_string(),
        "alpha/one".to_string(),
        "zeta/model".to_string(),
    ];
    let out = catalog_model_ids(&live, "mid/two");
    assert_eq!(
        out,
        vec![
            "alpha/one".to_string(),
            "mid/two".to_string(),
            "zeta/model".to_string()
        ],
        "live ids dedupe and sort; an omitted current id is appended so the picker can show it"
    );

    // A current id already present is not duplicated.
    let out = catalog_model_ids(&live, "alpha/one");
    assert_eq!(out.iter().filter(|id| **id == "alpha/one").count(), 1);

    // No current id: nothing appended.
    let out = catalog_model_ids(&live, "");
    assert_eq!(out.len(), 2);
}

#[test]
fn fallback_lists_match_each_backend_and_stay_sorted() {
    assert_eq!(
        fallback_model_ids("openrouter", ""),
        [
            "deepseek/deepseek-v4-flash".to_string(),
            "deepseek/deepseek-v4-pro".to_string()
        ],
        "the raw list comes back A–Z so the picker order is stable"
    );
    assert_eq!(
        fallback_model_ids("deepseek", ""),
        ["deepseek-chat".to_string(), "deepseek-reasoner".to_string()]
    );
    assert_eq!(
        fallback_model_ids("openai", ""),
        ["gpt-4o".to_string(), "gpt-4o-mini".to_string()]
    );
    // An unknown backend falls back to the OpenRouter list, never empty: the picker must
    // show something even before any key is configured.
    assert_eq!(fallback_model_ids("mystery", "").len(), 2);

    let with_current = fallback_model_ids("deepseek", "custom/slug");
    assert!(
        with_current.contains(&"custom/slug".to_string()),
        "{with_current:?}"
    );
    let mut sorted = with_current.clone();
    sorted.sort();
    assert_eq!(with_current, sorted);
}

#[test]
fn display_names_keep_the_full_slug_and_descriptions_name_the_backend() {
    assert_eq!(
        display_name_for("deepseek/deepseek-v4-pro"),
        "deepseek/deepseek-v4-pro"
    );
    assert_eq!(description_for("openrouter", "a/b"), "OpenRouter · a/b");
    assert_eq!(description_for("deepseek", "a/b"), "DeepSeek API · a/b");
    assert_eq!(description_for("openai", "a/b"), "OpenAI · a/b");
    assert_eq!(description_for("topology-x", "a/b"), "topology-x · a/b");
}

#[tokio::test]
async fn missing_key_provider_refuses_completion_but_serves_a_catalog() {
    let p = MissingKeyProvider {
        model: std::sync::RwLock::new("start/model".into()),
    };
    assert_eq!(p.model(), "start/model");

    // set_model trims and refuses to blank the model.
    p.set_model("  next/model  ".into());
    assert_eq!(p.model(), "next/model");
    p.set_model("   ".into());
    assert_eq!(p.model(), "next/model");

    let models = p.list_models().await.expect("keyless picker still lists");
    assert_eq!(
        models,
        OPENROUTER_FALLBACK_RAW
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );

    let err = p
        .complete(CompletionRequest::new(Vec::new()))
        .await
        .expect_err("no key means no completion");
    match &err {
        ProviderError::InvalidRequest(msg) => {
            assert!(msg.contains("OPENROUTER_API_KEY"), "{msg}");
        }
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
}
