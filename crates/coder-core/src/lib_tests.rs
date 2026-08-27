//! Split from `lib.rs` for module-health boundaries.

use super::*;

fn role(model: &str) -> CoderRoleConfig {
    CoderRoleConfig {
        model: model.to_string(),
        prompt_path: Some(format!("prompts/{model}.md")),
        prompt: None,
        temperature: Some(0.1),
        max_tokens: Some(4096),
        max_turns: Some(8),
        reasoning: None,
    }
}

#[test]
fn run_request_round_trips_json() {
    let request = CoderRunRequest {
        task: CoderTask::new("task-1", "add a copy button").with_context("webui chat"),
        workspace: WorkspaceRef::new("C:/repo", "main"),
        config: CoderRunConfig {
            backend: LIBERADO_LOOP_BACKEND.to_string(),
            trace_dir: Some("coder-traces".to_string()),
            trace_formats: Vec::new(),
            planner: role("deepseek/deepseek-v4-pro"),
            coder: role("deepseek/deepseek-v4-pro"),
            critic: role("deepseek/deepseek-v4-flash"),
            gate: CoderGateConfig::default(),
            repair: None,
            sandbox: SandboxSpec::HostLocal,
            command_policy: CommandPolicy::default(),
            verifiers: Vec::new(),
            verify_policy: PipelinePolicy::default(),
            validation_command: Some(CoderCommandConfig {
                program: "cargo".to_string(),
                args: vec!["test".to_string()],
                env: std::collections::BTreeMap::new(),
                timeout_secs: Some(300),
                output_max_bytes: Some(4096),
            }),
            path_policy: PathPolicy::default(),
            progress: ProgressPolicy::default(),
            hashline: HashlineConfig::default(),
            session_critic: SessionCriticConfig::default(),
            prompt_dir: None,
            edit: Default::default(),
            workspace_build: Default::default(),
            offered_tools: None,
        },
        attempt: 0,
        prior_feedback: Vec::new(),
        strategist_directive: None,
    };

    let json = serde_json::to_string_pretty(&request).unwrap();
    let back: CoderRunRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(request, back);
}

#[test]
fn sandbox_worktree_round_trips_json() {
    let spec = SandboxSpec::Worktree;
    let json = serde_json::to_string(&spec).unwrap();
    assert_eq!(json, r#"{"backend":"worktree"}"#);
    let back: SandboxSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(back, SandboxSpec::Worktree);
}

#[test]
fn coder_result_converts_to_report() {
    let result = CoderRunResult {
        backend: LIBERADO_LOOP_BACKEND.to_string(),
        outcome: Outcome::Succeeded,
        summary: "Added copy button".to_string(),
        files_changed: vec!["crates/webui/src/components/chat.rs".to_string()],
        file_changes: Vec::new(),
        validation_notes: Some("cargo check passed".to_string()),
        critic_verdict: Some(CriticVerdict::Acceptable),
        gate_votes: Vec::new(),
        trace_path: Some("traces/task-1.jsonl".to_string()),
        diff_findings: Vec::new(),
        session_findings: Vec::new(),
        remediation: None,
        diagnostics: serde_json::json!({"turns": 5}),
    };

    let report = result.report();
    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(report.artifacts, result.files_changed);
    assert!(report.summary.contains("copy button"));
}

#[test]
fn gate_config_default_is_disabled() {
    let gate = CoderGateConfig::default();
    assert!(!gate.enabled);
    assert_eq!(gate.fresh_reviewers, 2);
    assert_eq!(gate.strategist_after, 3);
    assert!(gate.gatekeeper.is_none());
    assert!(gate.fresh.is_none());
    assert!(gate.strategist.is_none());
}

/// The default was `enabled: false, hash_length: 4`. It is now off, at length 7 — see
/// `HashlineConfig::default` for the run that changed it. The values live in
/// `hashline_default_tests`, which says *why* each one is what it is; this test keeps only
/// the part that belongs here: whatever the default is, it must pass its own validator.
#[test]
fn the_default_hashline_config_validates() {
    assert!(HashlineConfig::default().validate().is_ok());
}

#[test]
fn hashline_config_rejects_out_of_range_length() {
    assert!(
        HashlineConfig {
            enabled: true,
            hash_length: 3,
        }
        .validate()
        .is_err()
    );
    assert!(
        HashlineConfig {
            enabled: false,
            hash_length: 11,
        }
        .validate()
        .is_err()
    );
    assert!(
        HashlineConfig {
            enabled: true,
            hash_length: 10,
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn hashline_config_accepts_every_length_in_range() {
    for len in HashlineConfig::HASH_LENGTH_MIN..=HashlineConfig::HASH_LENGTH_MAX {
        assert!(
            HashlineConfig {
                enabled: true,
                hash_length: len,
            }
            .validate()
            .is_ok(),
            "length {len}"
        );
    }
}

#[test]
fn hashline_config_round_trips_json() {
    let cfg = HashlineConfig {
        enabled: true,
        hash_length: 7,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: HashlineConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(cfg, back);
}

#[test]
fn coder_run_config_deserializes_absent_hashline_as_default() {
    // Minimal JSON without hashline key — serde default must fill it.
    let json = r#"{
            "backend": "liberado-loop",
            "planner": {"model": "m", "prompt": "p", "max_turns": 1},
            "coder": {"model": "m", "prompt": "p", "max_turns": 1},
            "critic": {"model": "m", "prompt": "p", "max_turns": 1}
        }"#;
    let cfg: CoderRunConfig = serde_json::from_str(json).unwrap();
    // An absent `[coder.hashline]` must land on the *same* default a caller gets from
    // `HashlineConfig::default()`. Comparing against the type, not against literals, is what
    // stops this test from having to be edited every time the default is retuned — and it is
    // the property the test is actually about: deserialization must not invent its own.
    assert_eq!(cfg.hashline, HashlineConfig::default());
}

#[test]
fn plan_mode_coder_prompt_is_non_empty() {
    assert!(!PLAN_MODE_CODER_PROMPT.is_empty());
    assert!(PLAN_MODE_CODER_PROMPT.contains(".liberado/plan.md"));
}

#[test]
fn explore_mode_coder_prompt_is_non_empty() {
    assert!(!EXPLORE_MODE_CODER_PROMPT.is_empty());
    assert!(EXPLORE_MODE_CODER_PROMPT.contains("read-only"));
}

#[test]
fn explore_tool_names_are_write_free() {
    assert!(EXPLORE_TOOL_NAMES.contains(&"list_files"));
    assert!(EXPLORE_TOOL_NAMES.contains(&"read_file"));
    assert!(!EXPLORE_TOOL_NAMES.contains(&"write_file"));
    assert!(!EXPLORE_TOOL_NAMES.contains(&"edit_file"));
}

#[test]
fn command_policy_none_allowed_denies_everything() {
    let p = CommandPolicy::none_allowed();
    assert!(
        !p.allow.is_empty(),
        "non-empty allow list with sentinel blocks all commands"
    );
    assert_eq!(p.output_max_bytes, 64 * 1024);
    assert_eq!(p.timeout_secs, 120);
}

#[test]
fn path_policy_plan_mode_restricts_to_plan_artifact() {
    let p = PathPolicy::plan_mode();
    assert_eq!(p.allow_write_globs, vec![PLAN_ARTIFACT_REL]);
    assert!(!p.writes_disabled());
}

#[test]
fn path_policy_accepts_a_dispatch_write_scope_without_other_fields() {
    let policy: PathPolicy = serde_json::from_value(serde_json::json!({
        "write_scope": { "allow_globs": ["docs/**"] }
    }))
    .expect("scope-only path policy");
    assert_eq!(policy.allow_write_globs, vec!["**"]);
    assert_eq!(policy.deny_globs, PathPolicy::default().deny_globs);
    assert!(policy.write_scope.is_active());
    assert!(policy.write_scope.permits("docs/guide.md")); // docs-check: ignore
    assert!(!policy.write_scope.permits("src/main.rs"));
}

#[test]
fn path_policy_read_only_disables_all_writes() {
    let p = PathPolicy::read_only();
    assert!(p.allow_write_globs.is_empty());
    assert!(p.writes_disabled());
}

#[test]
fn path_policy_writes_disabled_when_no_globs() {
    let mut p = PathPolicy::default();
    assert!(!p.writes_disabled());
    p.allow_write_globs.clear();
    assert!(p.writes_disabled());
}

#[test]
fn path_policy_writes_not_disabled_when_globs_present() {
    let p = PathPolicy::default();
    assert!(!p.writes_disabled());
}
