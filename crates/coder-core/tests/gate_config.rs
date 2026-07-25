#[test]
fn documented_gate_block_parses_into_tuning() {
    let toml_src = r#"
[gate]
enabled = false
fresh_reviewers = 2
strategist_after = 3
[gate.gatekeeper]
model = "deepseek/deepseek-v4-pro"
prompt_path = "prompts/coder/critic.md"
"#;
    let value: toml::Value = toml_src.parse().unwrap();
    let tuning = liberado_coder_core::CoderTuning::from_value(Some(&value)).unwrap();
    assert_eq!(tuning.gate.fresh_reviewers, 2);
    assert_eq!(tuning.gate.strategist_after, 3);
    assert!(!tuning.gate.enabled);
    assert_eq!(
        tuning.gate.gatekeeper.as_ref().unwrap().model,
        "deepseek/deepseek-v4-pro"
    );
}

#[test]
fn an_enabled_gate_with_no_reviewers_is_rejected_at_load_time() {
    let toml_src = "[gate]\nenabled = true\nfresh_reviewers = 0\n";
    let value: toml::Value = toml_src.parse().unwrap();
    let err = liberado_coder_core::CoderTuning::from_value(Some(&value))
        .expect_err("a gate that can never approve must not load");
    assert!(err.to_string().contains("fresh_reviewers"), "{err}");
}
