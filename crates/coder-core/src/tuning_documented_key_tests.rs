//! Split from `tuning.rs` for module-health boundaries.

use super::*;

/// Every section `config.example/tuning.toml` documents must actually parse.
///
/// This is the ninth-and-a-half shadowed setting: `[coder.workspace]` was documented in the
/// example, in the struct's own doc comment, and in a merged PR description — while serde
/// looked for `[coder.workspace_build]`. It parsed, defaulted, and changed nothing. A run
/// configured with a shared build cache filled its own worktree with 17.6 GB and left the
/// shared directory at zero.
///
/// `CoderTuning` already has a test proving each field survives the conversion to
/// `CoderRunConfig`. That cannot see this: the value never entered the struct in the first
/// place. The gap between "the code reads it" and "the operator can write it" needs its own
/// check, and the only honest source for the operator's half is the file they are told to
/// copy.
#[test]
fn the_documented_workspace_section_reaches_the_config() {
    let toml = r#"
[coder.workspace]
shared_target_dir = "/tmp/shared"
warmup = false
warmup_timeout_secs = 42
"#;
    let value: toml::Value = toml.parse().expect("valid toml");
    let coder = value.get("coder").expect("[coder] table");
    let tuning = CoderTuning::from_value(Some(coder)).expect("tuning parses");

    assert_eq!(
        tuning.workspace_build.shared_target_dir.as_deref(),
        Some("/tmp/shared"),
        "the key an operator is told to write must reach the field"
    );
    assert!(!tuning.workspace_build.warmup);
    assert_eq!(tuning.workspace_build.warmup_timeout_secs, 42);
}

/// And the same for the other sections added this week, since each one was documented before
/// anyone tried writing it.
#[test]
fn the_documented_edit_and_hashline_sections_reach_the_config() {
    let toml = r#"
[coder.edit]
fuzzy_match = false
fuzzy_threshold = 0.8

[coder.hashline]
enabled = true
hash_length = 5
"#;
    let value: toml::Value = toml.parse().expect("valid toml");
    let coder = value.get("coder").expect("[coder] table");
    let tuning = CoderTuning::from_value(Some(coder)).expect("tuning parses");

    assert!(
        !tuning.edit.fuzzy_match,
        "[coder.edit] did not reach the field"
    );
    assert_eq!(tuning.edit.fuzzy_threshold, 0.8);
    assert!(
        tuning.hashline.enabled,
        "[coder.hashline] did not reach the field"
    );
    assert_eq!(tuning.hashline.hash_length, 5);
}

/// `[coder] prompt_dir` is a bare key rather than a section, and was never exercised either.
#[test]
fn the_documented_prompt_dir_reaches_the_config() {
    let value: toml::Value = "prompt_dir = \"/etc/liberado/prompts\"\n"
        .parse()
        .expect("valid toml");
    let tuning = CoderTuning::from_value(Some(&value)).expect("tuning parses");
    assert_eq!(tuning.prompt_dir.as_deref(), Some("/etc/liberado/prompts"));
}

/// The compare-2 operator file: four tools and Flash thinking, no restated prompt path.
///
/// Serde replaces the whole `[coder.coder]` role, so `prompt_path` became `None` and
/// `validate` rejected the section. Loaders then defaulted and the model saw 21 tools
/// with no reasoning. A table that only names the knobs the operator wants to change
/// must still parse.
#[test]
fn a_partial_coder_role_keeps_offered_tools_and_reasoning() {
    let value: toml::Value = r#"
offered_tools = ["read_file", "write_file", "edit_file", "run_command"]

[coder]
model = "deepseek/deepseek-v4-flash"
temperature = 0.1
max_turns = 30
reasoning = "high"
"#
    .parse()
    .expect("valid toml");
    let tuning = CoderTuning::from_value(Some(&value)).expect("partial [coder.coder] must parse");
    assert_eq!(
        tuning.offered_tools.as_deref(),
        Some(
            [
                "read_file".to_string(),
                "write_file".to_string(),
                "edit_file".to_string(),
                "run_command".to_string()
            ]
            .as_slice()
        )
    );
    assert_eq!(tuning.coder.model, "deepseek/deepseek-v4-flash");
    assert_eq!(tuning.coder.reasoning.as_deref(), Some("high"));
    assert_eq!(
        tuning.coder.prompt_path.as_deref(),
        default_coder_role().prompt_path.as_deref(),
        "omitting prompt_path must keep the default coder prompt, not fail validation"
    );
}

/// The live operator file that only pins the critic model. Compare 5's
/// `stdio_smoke` died on walk-up into this shape: `max_turns` became `None`,
/// validate reported "must be >= 1".
#[test]
fn a_partial_critic_role_keeps_default_max_turns() {
    let value: toml::Value = r#"
[critic]
model = "deepseek/deepseek-v4-flash"
"#
    .parse()
    .expect("valid toml");
    let tuning = CoderTuning::from_value(Some(&value)).expect("partial [coder.critic] must parse");
    assert_eq!(tuning.critic.model, "deepseek/deepseek-v4-flash");
    assert_eq!(
        tuning.critic.max_turns,
        default_coder_critic().max_turns,
        "omitting critic max_turns must keep the default, not fail validation"
    );
    assert_eq!(
        tuning.critic.prompt_path.as_deref(),
        default_coder_critic().prompt_path.as_deref()
    );
}

#[test]
fn an_explicit_coder_prompt_is_not_replaced_by_the_default_path() {
    let value: toml::Value = r#"
[coder]
model = "deepseek-v4-flash"
prompt = "you are a fixture"
max_turns = 8
"#
    .parse()
    .expect("valid toml");
    let tuning = CoderTuning::from_value(Some(&value)).expect("inline prompt must parse");
    assert_eq!(tuning.coder.prompt.as_deref(), Some("you are a fixture"));
    assert!(
        tuning.coder.prompt_path.is_none(),
        "an inline prompt must not grow a default prompt_path"
    );
}
