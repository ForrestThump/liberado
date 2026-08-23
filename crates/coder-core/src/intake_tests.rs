//! Split from `intake.rs` for module-health boundaries.

use super::*;

#[test]
fn verifiers_skip_unknown_type_entries() {
    let raw = r#"{
            "status": "ready_for_freeze",
            "draft": {
                "description": "todo",
                "success_criteria": ["works"],
                "verifiers": [
                    {"type": "verify_profile", "name": "rust-check"},
                    {"type": "paths_exist", "paths": ["src/main.rs"]},
                    {"type": "not_a_real_kind", "id": "x"}
                ]
            }
        }"#;
    let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
    match outcome {
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            assert_eq!(draft.verifiers.len(), 1);
            assert_eq!(draft.verifiers[0].id(), "paths_exist");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn success_criteria_accepts_single_string() {
    let raw = r#"{
            "status": "ready_for_freeze",
            "draft": {
                "description": "todo cli",
                "success_criteria": "add and list work",
                "verifiers": [],
                "out_of_scope": "no network",
                "assumed_defaults": ["Rust"]
            },
            "rationale": "ok"
        }"#;
    let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
    match outcome {
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            assert_eq!(
                draft.success_criteria,
                vec!["add and list work".to_string()]
            );
            assert_eq!(draft.out_of_scope, vec!["no network".to_string()]);
            assert_eq!(draft.assumed_defaults, vec!["Rust".to_string()]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn freeze_rejects_empty_description() {
    let draft = GoalContractDraft {
        description: "  ".into(),
        success_criteria: vec![],
        verifiers: vec![],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    };
    assert!(GoalContract::freeze("g1", draft, FreezeAuthority::Human).is_err());
}

#[test]
fn freeze_stamps_hash() {
    let draft = GoalContractDraft {
        description: "Build a todo CLI".into(),
        success_criteria: vec!["add and list work".into()],
        verifiers: vec![VerifierSpec::PathsExist {
            id: "paths".into(),
            paths: vec!["src/main.rs".into()],
        }],
        out_of_scope: vec![],
        assumed_defaults: vec!["Rust".into()],
        domain_hint: Some("coding".into()),
        verify_profile: Some("rust-check".into()),
    };
    let c = GoalContract::freeze("g1", draft, FreezeAuthority::Human).unwrap();
    // A real SHA-256, per verifiers.md §7 — 64 hex chars behind a `sha256:` label.
    assert!(
        c.content_hash.starts_with("sha256:"),
        "got {}",
        c.content_hash
    );
    let digest = c.content_hash.strip_prefix("sha256:").unwrap();
    assert_eq!(digest.len(), 64);
    assert!(digest.chars().all(|ch| ch.is_ascii_hexdigit()));
    // Structural check + expanded rust-check profile (cargo-check).
    assert_eq!(c.draft.verifiers.len(), 2);
    assert!(c.draft.verifiers.iter().any(|v| v.id() == "cargo-check"));
    // A freshly frozen contract verifies against itself.
    c.verify_integrity().unwrap();
}

fn contract_for_tamper_tests() -> GoalContract {
    GoalContract::freeze(
        "g1",
        GoalContractDraft {
            description: "Build a todo CLI".into(),
            success_criteria: vec!["add and list work".into()],
            verifiers: vec![VerifierSpec::Command {
                id: "cargo-test".into(),
                program: "cargo".into(),
                args: vec!["test".into()],
                env: Default::default(),
                timeout_secs: None,
                output_max_bytes: None,
                network: false,
            }],
            out_of_scope: vec![],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: None,
        },
        FreezeAuthority::Human,
    )
    .unwrap()
}

#[test]
fn weakening_the_gates_after_freeze_is_detected() {
    // The attack the hash exists to catch: the contract said "cargo test must pass"; something
    // downstream quietly drops the gate so the work grades itself as done. The contract must no
    // longer verify — otherwise "frozen" means nothing.
    let mut c = contract_for_tamper_tests();
    c.verify_integrity().expect("pristine contract verifies");

    c.draft.verifiers.clear();
    let err = c
        .verify_integrity()
        .expect_err("dropped gates must be caught");
    assert!(err.contains("modified since it was frozen"), "got: {err}");
}

#[test]
fn rewriting_the_goal_after_freeze_is_detected() {
    // The other half: the gates survive but the goal is swapped underneath them.
    let mut c = contract_for_tamper_tests();
    c.draft.description = "Build something else entirely".into();
    assert!(c.verify_integrity().is_err());
}

#[test]
fn the_hash_is_stable_across_freezes_of_the_same_draft() {
    // `frozen_at` differs between these two, but the hash covers the *draft*, not the stamp —
    // so the same agreed content always has the same identity. (The old DefaultHasher was not
    // even stable across Rust releases, which would have made a stored contract fail to verify
    // after a toolchain bump, for no reason at all.)
    let a = contract_for_tamper_tests();
    let b = contract_for_tamper_tests();
    assert_eq!(a.content_hash, b.content_hash);
}

// ── StringOrVec visitor shapes ────────────────────────────────────────────
// success_criteria, out_of_scope, and assumed_defaults use a custom serde
// visitor that accepts many JSON shapes a model might emit.

#[test]
fn success_criteria_as_boolean() {
    let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":true,"verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
    let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
    match outcome {
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            assert_eq!(draft.success_criteria, vec!["true"]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn success_criteria_as_number() {
    let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":42,"verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
    let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
    match outcome {
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            assert_eq!(draft.success_criteria, vec!["42"]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn success_criteria_as_array_of_strings() {
    let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":["a","b"],"verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
    let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
    match outcome {
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            assert_eq!(draft.success_criteria, vec!["a", "b"]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn success_criteria_as_array_of_mixed_types() {
    let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":["a",42,true,null,{}],"verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
    let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
    match outcome {
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            assert_eq!(draft.success_criteria, vec!["a", "42", "true", "{}"]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn success_criteria_as_null() {
    let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":null,"verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
    let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
    match outcome {
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            assert!(draft.success_criteria.is_empty());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn success_criteria_as_empty_string() {
    let raw = r#"{"status":"ready_for_freeze","draft":{"description":"x","success_criteria":"   ","verifiers":[],"out_of_scope":"","assumed_defaults":""}}"#;
    let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
    match outcome {
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            assert!(draft.success_criteria.is_empty());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn sanitize_draft_drops_incomplete_verifiers() {
    let mut draft = GoalContractDraft {
        description: "x".into(),
        success_criteria: vec![],
        verifiers: vec![
            VerifierSpec::Command {
                id: "empty".into(),
                program: String::new(),
                args: vec![],
                env: Default::default(),
                timeout_secs: None,
                output_max_bytes: None,
                network: false,
            },
            VerifierSpec::PathsExist {
                id: "p".into(),
                paths: vec![],
            },
            VerifierSpec::ContentContains {
                id: "bad-cc".into(),
                path: String::new(),
                must_include: vec!["something".into()],
            },
            VerifierSpec::GitNonemptyDiff { id: "diff".into() },
        ],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    };
    sanitize_draft(&mut draft);
    assert_eq!(draft.verifiers.len(), 1);
    assert_eq!(draft.verifiers[0].id(), "diff");
}

#[test]
fn validate_draft_content_contains_needs_fields() {
    let draft = GoalContractDraft {
        description: "x".into(),
        success_criteria: vec![],
        verifiers: vec![VerifierSpec::ContentContains {
            id: "c".into(),
            path: String::new(),
            must_include: vec![String::new()],
        }],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    };
    assert!(validate_draft(&draft).is_err());
}

#[test]
fn node_test_profile_resolves() {
    let profile = profile_verifiers("node-test");
    assert!(profile.iter().any(|v| v.id() == "npm-test"));
}

/// Dogfood finding #2: DeepSeek under json_object fallback emitted `prompt` as a sequence.
#[test]
fn question_prompt_accepts_string_array() {
    let raw = r#"{
            "status": "needs_clarification",
            "questions": [
                {
                    "id": "workspace_path",
                    "prompt": ["What is the absolute path?", "Please provide it."],
                    "options": ["a", "b"]
                }
            ]
        }"#;
    let outcome: IntakeOutcome = serde_json::from_str(raw).unwrap();
    match outcome {
        IntakeOutcome::NeedsClarification { questions, .. } => {
            assert_eq!(questions.len(), 1);
            assert!(questions[0].prompt.contains("absolute path"));
            assert!(questions[0].prompt.contains("Please provide"));
            assert_eq!(questions[0].options, vec!["a", "b"]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn profile_verifiers_known_profiles() {
    assert!(!profile_verifiers("rust-check").is_empty());
    assert!(!profile_verifiers("rust-strict").is_empty());
    assert!(!profile_verifiers("node-test").is_empty());
}

#[test]
fn profile_verifiers_unknown_returns_empty() {
    assert!(profile_verifiers("").is_empty());
    assert!(profile_verifiers("bogus").is_empty());
    assert!(profile_verifiers("  unknown  ").is_empty());
}

#[test]
fn sanitize_draft_drops_empty_command_verifier() {
    let mut draft = GoalContractDraft {
        description: "test".into(),
        success_criteria: vec![],
        verifiers: vec![
            VerifierSpec::Command {
                id: "cmd".into(),
                program: String::new(),
                args: vec![],
                env: Default::default(),
                timeout_secs: None,
                output_max_bytes: None,
                network: false,
            },
            VerifierSpec::Command {
                id: "ok".into(),
                program: "echo".into(),
                args: vec![],
                env: Default::default(),
                timeout_secs: None,
                output_max_bytes: None,
                network: false,
            },
        ],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    };
    sanitize_draft(&mut draft);
    assert_eq!(draft.verifiers.len(), 1, "empty program should be dropped");
    assert_eq!(draft.verifiers[0].id(), "ok");
}

#[test]
fn sanitize_draft_drops_paths_exist_with_no_paths() {
    let mut draft = GoalContractDraft {
        description: "test".into(),
        success_criteria: vec![],
        verifiers: vec![VerifierSpec::PathsExist {
            id: "pe".into(),
            paths: vec![],
        }],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    };
    sanitize_draft(&mut draft);
    assert!(draft.verifiers.is_empty());
}

#[test]
fn sanitize_draft_drops_paths_absent_with_no_paths() {
    let mut draft = GoalContractDraft {
        description: "test".into(),
        success_criteria: vec![],
        verifiers: vec![VerifierSpec::PathsAbsent {
            id: "pa".into(),
            paths: vec![],
        }],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    };
    sanitize_draft(&mut draft);
    assert!(draft.verifiers.is_empty());
}

#[test]
fn sanitize_draft_drops_content_contains_with_empty_path() {
    let mut draft = GoalContractDraft {
        description: "test".into(),
        success_criteria: vec![],
        verifiers: vec![VerifierSpec::ContentContains {
            id: "cc".into(),
            path: String::new(),
            must_include: vec!["needed".into()],
        }],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    };
    sanitize_draft(&mut draft);
    assert!(draft.verifiers.is_empty());
}

#[test]
fn sanitize_draft_drops_content_contains_with_empty_must_include() {
    let mut draft = GoalContractDraft {
        description: "test".into(),
        success_criteria: vec![],
        verifiers: vec![VerifierSpec::ContentContains {
            id: "cc".into(),
            path: "README.md".into(),
            must_include: vec![],
        }],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    };
    sanitize_draft(&mut draft);
    assert!(
        draft.verifiers.is_empty(),
        "verifier with non-empty path but empty must_include should be dropped"
    );
}

#[test]
fn sanitize_draft_keeps_git_diff_verifier() {
    let mut draft = GoalContractDraft {
        description: "test".into(),
        success_criteria: vec![],
        verifiers: vec![VerifierSpec::GitNonemptyDiff { id: "diff".into() }],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    };
    sanitize_draft(&mut draft);
    assert_eq!(draft.verifiers.len(), 1);
}

#[test]
fn validate_draft_rejects_content_contains_without_must_include() {
    let draft = GoalContractDraft {
        description: "test".into(),
        success_criteria: vec![],
        verifiers: vec![VerifierSpec::ContentContains {
            id: "cc".into(),
            path: "README.md".into(),
            must_include: vec![],
        }],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    };
    assert!(validate_draft(&draft).is_err());
}

#[test]
fn apply_to_request_populates_task_and_config() {
    let now = chrono::Utc::now();
    let draft = GoalContractDraft {
        description: "add feature".into(),
        success_criteria: vec!["test passes".into()],
        verifiers: vec![VerifierSpec::GitNonemptyDiff { id: "diff".into() }],
        out_of_scope: vec!["no db".into()],
        assumed_defaults: vec!["Rust".into()],
        domain_hint: None,
        verify_profile: None,
    };
    let content_hash = hash_draft(&draft);
    let contract = GoalContract {
        id: "g1".into(),
        draft,
        frozen_at: now,
        frozen_by: FreezeAuthority::Human,
        content_hash,
    };
    let empty_role = crate::CoderRoleConfig {
        model: String::new(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: None,
        reasoning: None,
    };
    let mut request = crate::CoderRunRequest {
        task: crate::CoderTask::new("x", "old"),
        workspace: crate::WorkspaceRef::new("/tmp", "main"),
        config: crate::CoderRunConfig {
            backend: String::new(),
            trace_dir: None,
            trace_formats: Vec::new(),
            planner: empty_role.clone(),
            coder: empty_role.clone(),
            critic: empty_role.clone(),
            gate: Default::default(),
            repair: None,
            sandbox: Default::default(),
            command_policy: Default::default(),
            validation_command: Some(crate::CoderCommandConfig::new("legacy")),
            verifiers: vec![],
            verify_policy: Default::default(),
            path_policy: Default::default(),
            progress: Default::default(),
            hashline: Default::default(),
            session_critic: crate::SessionCriticConfig::default(),
            prompt_dir: None,
            edit: Default::default(),
            workspace_build: Default::default(),
            offered_tools: None,
        },
        attempt: 0,
        prior_feedback: vec![],
        strategist_directive: None,
    };
    contract.apply_to_request(&mut request);
    assert_eq!(request.task.description, "add feature");
    assert_eq!(request.task.success_criteria, vec!["test passes"]);
    assert_eq!(request.config.verifiers.len(), 1);
    assert_eq!(request.config.verifiers[0].id(), "diff");
    assert!(
        request.config.validation_command.is_none(),
        "validation_command should be cleared when verifiers present"
    );
}

#[test]
fn intake_outcome_schema_has_expected_shape() {
    let schema = intake_outcome_schema();
    assert_eq!(schema["type"], "object");
    let required = schema["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v == "status"));
    let props = &schema["properties"];
    assert!(
        props["status"]["enum"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("needs_clarification"))
    );
}
