//! Split from `tuning.rs` for module-health boundaries.

use super::*;

#[test]
fn absent_section_yields_validated_defaults() {
    let tuning = CoderTuning::from_value(None).unwrap();
    assert_eq!(tuning.backend, LIBERADO_LOOP_BACKEND);
    assert_eq!(tuning.trace_dir.as_deref(), Some("coder-traces"));
}

#[test]
fn parses_overrides_from_raw_value() {
    let value: toml::Value = toml::from_str(
        r#"
            [coder]
            model = "deepseek-v4-pro"
            prompt_path = "prompts/custom-coder.md"
            max_turns = 44

            [progress]
            read_only_turn_limit = 5
            same_tool_limit = 4
            validation_repeat_limit = 3
            max_attempts = 2
            event_preview_max_chars = 321
            "#,
    )
    .unwrap();
    let tuning = CoderTuning::from_value(Some(&value)).unwrap();
    assert_eq!(
        tuning.coder.prompt_path.as_deref(),
        Some("prompts/custom-coder.md")
    );
    assert_eq!(tuning.coder.max_turns, Some(44));
    assert_eq!(tuning.progress.event_preview_max_chars, 321);
}

#[test]
fn parses_a_scope_only_path_policy_with_base_defaults() {
    let value: toml::Value = toml::from_str(
        r#"
            [path_policy.write_scope]
            allow_globs = ["docs/**"]
            deny_globs = ["docs/private/**"]
            "#,
    )
    .unwrap();
    let tuning = CoderTuning::from_value(Some(&value)).unwrap();
    assert_eq!(tuning.path_policy.allow_write_globs, vec!["**"]);
    assert!(tuning.path_policy.write_scope.permits("docs/guide.md")); // docs-check: ignore
    assert!(!tuning.path_policy.write_scope.permits("src/main.rs"));
}

#[test]
fn validation_rejects_missing_role_budget() {
    let mut tuning = CoderTuning::default();
    tuning.coder.max_turns = None;
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("tuning.coder.coder.max_turns"));
}

#[test]
fn validation_rejects_zero_preview_cap() {
    let mut tuning = CoderTuning::default();
    tuning.progress.event_preview_max_chars = 0;
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("tuning.coder.progress"));
}

#[test]
fn run_config_clones_all_fields() {
    let tuning = CoderTuning::default();
    let config = tuning.run_config();
    assert_eq!(config.backend, tuning.backend);
    assert_eq!(config.planner.model, tuning.planner.model);
    assert_eq!(config.hashline, tuning.hashline);
}

/// Every field shared by `CoderTuning` and `CoderRunConfig` must actually carry across.
///
/// This enumerates fields via serde instead of naming them, which is the whole point: the test
/// above is called `run_config_clones_all_fields` and checks three of nineteen, so a field
/// added to both types and forgotten in `run_config` passes it. That is failure-mode class 7 —
/// a setting that parses, validates, and is never read — and it has happened **eight** times
/// here. `trace_dir` shipped with a default of `Some("coder-traces")`, a passing loader test,
/// and `None` hardcoded at every consumer, so the trace facility never once wrote a file.
///
/// Comparing the *default* tuning is deliberately not enough: a field whose default happens to
/// equal the hardcoded literal (`gate.enabled = false`) would agree by accident. So the tuning
/// is twisted away from its defaults first — booleans flipped, numbers bumped, absent options
/// filled — through JSON, so no field has to be named to be covered.
#[test]
fn every_shared_field_survives_the_conversion_to_run_config() {
    fn twist(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Bool(b) => *b = !*b,
            serde_json::Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    *v = serde_json::json!(u + 7);
                }
            }
            serde_json::Value::Object(map) => map.values_mut().for_each(twist),
            // Strings and arrays are left alone: many are enum tags or validated shapes
            // (`sandbox.backend`, verifier specs) where an arbitrary edit fails to
            // deserialize. The fields that have actually been shadowed are booleans, numbers
            // and options, which is what this covers.
            _ => {}
        }
    }

    let mut as_json = serde_json::to_value(CoderTuning::default()).expect("tuning serializes");
    twist(&mut as_json);
    // A twisted value out of its validated range is fine — fall back to defaults rather than
    // skipping the check entirely, so the test still compares every shared field.
    let tuning: CoderTuning = serde_json::from_value(as_json).unwrap_or_default();

    let tuning_json = serde_json::to_value(&tuning).expect("tuning serializes");
    let config_json = serde_json::to_value(tuning.run_config()).expect("config serializes");
    let (tuning_map, config_map) = match (&tuning_json, &config_json) {
        (serde_json::Value::Object(t), serde_json::Value::Object(c)) => (t, c),
        _ => panic!("both types must serialize as objects"),
    };

    // Fields that legitimately differ. Each needs a reason, not just a name.
    const EXEMPT: &[(&str, &str)] = &[
        // Resolved per run from the workspace, not copied from config.
        ("repo_map", "generated per run, not a passthrough setting"),
    ];

    let mut checked = 0;
    for (key, tuning_value) in tuning_map {
        let Some(config_value) = config_map.get(key) else {
            continue; // Not a shared field — `run_config` is allowed to be narrower.
        };
        if let Some((_, why)) = EXEMPT.iter().find(|(k, _)| k == key) {
            let _ = why;
            continue;
        }
        assert_eq!(
            config_value, tuning_value,
            "`{key}` is set in [coder] and does not reach CoderRunConfig — it parses,                  validates, and is read by nobody (failure-mode class 7). Either copy it in                  `run_config`, or add it to EXEMPT with the reason it differs."
        );
        checked += 1;
    }

    assert!(
        checked >= 10,
        "expected to check most of the shared surface, only compared {checked} field(s) —              the comparison is probably not seeing the fields it thinks it is"
    );
}

#[test]
fn parses_hashline_section() {
    let value: toml::Value = toml::from_str(
        r#"
            [hashline]
            enabled = true
            hash_length = 8
            "#,
    )
    .unwrap();
    let tuning = CoderTuning::from_value(Some(&value)).unwrap();
    assert!(tuning.hashline.enabled);
    assert_eq!(tuning.hashline.hash_length, 8);
}

#[test]
fn validation_rejects_hashline_length_out_of_range() {
    let mut tuning = CoderTuning::default();
    tuning.hashline.hash_length = 2;
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("hash_length"));
}

#[test]
fn validation_rejects_empty_backend() {
    let tuning = CoderTuning {
        backend: String::new(),
        ..CoderTuning::default()
    };
    assert!(tuning.validate().is_err());
}

#[test]
fn validation_rejects_zero_command_timeout() {
    let mut tuning = CoderTuning::default();
    tuning.command_policy.timeout_secs = 0;
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("command_policy.timeout_secs"));
}

#[test]
fn validation_rejects_zero_command_output_max_bytes() {
    let mut tuning = CoderTuning::default();
    tuning.command_policy.output_max_bytes = 0;
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("command_policy.output_max_bytes"));
}

#[test]
fn validation_rejects_zero_path_read_max_bytes() {
    let mut tuning = CoderTuning::default();
    tuning.path_policy.read_max_bytes = 0;
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("path_policy.read_max_bytes"));
}

#[test]
fn validation_rejects_zero_path_search_max_results() {
    let mut tuning = CoderTuning::default();
    tuning.path_policy.search_max_results = 0;
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("path_policy.search_max_results"));
}

#[test]
fn validation_rejects_gate_enabled_with_zero_reviewers() {
    let mut tuning = CoderTuning::default();
    tuning.gate.enabled = true;
    tuning.gate.fresh_reviewers = 0;
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("fresh_reviewers"));
}

#[test]
fn validation_allows_gate_disabled_with_zero_reviewers() {
    let mut tuning = CoderTuning::default();
    tuning.gate.enabled = false;
    tuning.gate.fresh_reviewers = 0;
    assert!(tuning.validate().is_ok());
}

#[test]
fn validation_allows_gate_enabled_with_reviewers() {
    let mut tuning = CoderTuning::default();
    tuning.gate.enabled = true;
    tuning.gate.fresh_reviewers = 1;
    // Need a complete role config for gatekeeper too.
    tuning.gate.gatekeeper = Some(CoderRoleConfig {
        model: "test-model".into(),
        prompt_path: None,
        prompt: Some("gatekeeper".into()),
        temperature: None,
        max_tokens: None,
        max_turns: None,
        reasoning: None,
    });
    assert!(tuning.validate().is_ok());
}

#[test]
fn validation_rejects_progress_zero_fields_individually() {
    let fields = [
        ("read_only_turn_limit", 0, 1, 1, 1, 1),
        ("same_tool_limit", 1, 0, 1, 1, 1),
        ("validation_repeat_limit", 1, 1, 0, 1, 1),
        ("max_attempts", 1, 1, 1, 0, 1),
        ("event_preview_max_chars", 1, 1, 1, 1, 0),
    ];
    for (name, read_only, same_tool, val_repeat, max_att, preview) in &fields {
        let tuning = CoderTuning {
            progress: ProgressPolicy {
                read_only_turn_limit: *read_only,
                same_tool_limit: *same_tool,
                validation_repeat_limit: *val_repeat,
                max_attempts: *max_att,
                event_preview_max_chars: *preview,
            },
            ..CoderTuning::default()
        };
        let err = tuning.validate().unwrap_err();
        assert!(
            err.to_string().contains("progress"),
            "progress field {name} = 0 should be rejected"
        );
    }
}

#[test]
fn validate_role_identity_rejects_empty_model() {
    let role = CoderRoleConfig {
        model: "  ".into(),
        prompt_path: None,
        prompt: Some("prompt".into()),
        temperature: None,
        max_tokens: None,
        max_turns: None,
        reasoning: None,
    };
    assert!(validate_role_identity("test", &role).is_err());
}

#[test]
fn validate_role_identity_rejects_empty_prompt_and_path() {
    let role = CoderRoleConfig {
        model: "m".into(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: None,
        reasoning: None,
    };
    assert!(validate_role_identity("test", &role).is_err());
}

#[test]
fn validate_role_identity_accepts_model_with_prompt() {
    let role = CoderRoleConfig {
        model: "m".into(),
        prompt_path: None,
        prompt: Some("p".into()),
        temperature: None,
        max_tokens: None,
        max_turns: None,
        reasoning: None,
    };
    assert!(validate_role_identity("test", &role).is_ok());
}

#[test]
fn validate_single_shot_role_delegates_to_role_identity() {
    let bad = CoderRoleConfig {
        model: String::new(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: None,
        reasoning: None,
    };
    assert!(validate_single_shot_role("x", &bad).is_err());
}

#[test]
fn repo_map_config_serde_disabled() {
    let value: toml::Value = toml::from_str(
        r#"
            enabled = false
            task_aware = true
            max_map_tokens = 500
            min_source_files = 50
            "#,
    )
    .unwrap();
    let cfg: RepoMapConfig = value.try_into().unwrap();
    assert!(!cfg.enabled);
    assert!(cfg.task_aware);
    assert_eq!(cfg.max_map_tokens, 500);
    assert_eq!(cfg.min_source_files, 50);
}

#[test]
fn repo_map_absent_in_tuning_uses_defaults() {
    let value: toml::Value = toml::from_str(
        r#"
            [coder]
            model = "test"
            prompt = "p"
            max_turns = 1
            "#,
    )
    .unwrap();
    let tuning = CoderTuning::from_value(Some(&value)).unwrap();
    assert!(tuning.repo_map.enabled);
    assert!(!tuning.repo_map.task_aware);
    assert_eq!(tuning.repo_map.max_map_tokens, 1024);
    assert_eq!(tuning.repo_map.min_source_files, 20);
}

#[test]
fn validation_rejects_planner_zero_max_turns() {
    let mut tuning = CoderTuning::default();
    tuning.planner.max_turns = Some(0);
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("tuning.coder.planner.max_turns"));
}

#[test]
fn validation_rejects_critic_zero_max_turns() {
    let mut tuning = CoderTuning::default();
    tuning.critic.max_turns = Some(0);
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("tuning.coder.critic.max_turns"));
}

/// The built-in role models must be callable by the built-in provider.
///
/// The defaults were `deepseek/deepseek-v4-pro` — an aggregator-style slug — while the default
/// provider profile is DeepSeek's own API at `https://api.deepseek.com`, which answers:
///
/// > The supported API model names are deepseek-v4-pro or deepseek-v4-flash, but you passed
/// > deepseek/deepseek-v4-pro.
///
/// It went unnoticed because the session pack ignored the configured model entirely, so the
/// wrong default was never sent anywhere. The moment the model was honoured, every coding run
/// died on its first request.
///
/// A `/` is the specific tell: DeepSeek's own API takes a bare name, aggregators take
/// `vendor/model`. A deployment pointed at an aggregator should set the slug explicitly rather
/// than relying on these.
#[test]
fn default_role_models_are_bare_names_not_aggregator_slugs() {
    let t = CoderTuning::default();
    for (role, cfg) in [
        ("planner", &t.planner),
        ("coder", &t.coder),
        ("critic", &t.critic),
    ] {
        assert!(
            !cfg.model.contains('/'),
            "default {role} model `{}` is an aggregator slug; the default provider                  (api.deepseek.com) rejects it",
            cfg.model
        );
        assert!(
            !cfg.model.trim().is_empty(),
            "default {role} model is empty"
        );
    }
}

#[test]
fn validation_rejects_repair_bad_model() {
    let tuning = CoderTuning {
        repair: Some(CoderRoleConfig {
            model: String::new(),
            prompt_path: None,
            prompt: None,
            temperature: None,
            max_tokens: None,
            max_turns: None,
            reasoning: None,
        }),
        ..CoderTuning::default()
    };
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("tuning.coder.repair.model"));
}

#[test]
fn validation_rejects_gate_fresh_invalid() {
    let mut tuning = CoderTuning::default();
    tuning.gate.enabled = true;
    tuning.gate.fresh_reviewers = 1;
    tuning.gate.fresh = Some(CoderRoleConfig {
        model: String::new(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: None,
        reasoning: None,
    });
    let err = tuning.validate().unwrap_err();
    assert!(err.to_string().contains("tuning.coder.gate.fresh.model"));
}
