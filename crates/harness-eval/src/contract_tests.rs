//! Split from `contract.rs` for module-health boundaries.

use super::*;

fn spec() -> JobSpec {
    JobSpec {
        version: JOB_SPEC_VERSION,
        job_id: JobId::new(),
        submitted_at: Utc::now(),
        repository: PathBuf::from("C:/repo"),
        base_revision: "main".to_string(),
        task: TaskBundle::new("task.txt", "Fix the item".to_string()).unwrap(),
        harnesses: vec![
            HarnessRequest {
                id: "liberado".to_string(),
                binary: None,
                git_sha: None,
            },
            HarnessRequest {
                id: "pi".to_string(),
                binary: None,
                git_sha: None,
            },
        ],
        run_order: default_run_order(),
        model: ModelPins {
            provider: "openrouter".to_string(),
            model: "deepseek/test".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            credential_alias: "openrouter-default".to_string(),
            thinking: "high".to_string(),
            max_turns: 400,
            sampling: SAMPLING_OMITTED.to_string(),
        },
        limits: ResourceLimits::default(),
        verifier: VerifierProfile::WorkspaceTests,
        task_aware_context: true,
        acceptance: None,
        experiment: None,
        experiment_id: String::new(),
    }
    .finalize()
    .unwrap()
}

#[test]
fn immutable_pin_change_invalidates_experiment_id() {
    let mut value = spec();
    value.model.max_turns -= 1;
    assert!(value.validate().unwrap_err().contains("experiment id"));
}

#[test]
fn task_content_is_bound_to_its_digest() {
    let mut value = spec();
    value.task.text.push_str(" changed");
    assert!(value.validate().unwrap_err().contains("task digest"));
}

#[test]
fn verifier_repairs_are_opt_in_for_fair_comparisons() {
    assert_eq!(ResourceLimits::default().verifier_repair_attempts, 0);
}

#[test]
fn sampling_pin_rejects_values_not_applied_by_either_client() {
    let mut value = spec();
    value.model.sampling = "0.1".to_string();
    assert!(value.validate().unwrap_err().contains("sampling"));
}

#[test]
fn run_order_must_be_a_permutation_of_harness_ids() {
    let mut value = spec();
    value.run_order = vec!["pi".to_string()];
    assert!(
        value
            .validate()
            .unwrap_err()
            .contains("run_order must be a permutation")
    );

    let mut value = spec();
    value.run_order = vec!["pi".to_string(), "pi".to_string()];
    assert!(
        value
            .validate()
            .unwrap_err()
            .contains("run_order must be a permutation")
    );
}

#[test]
fn run_order_is_not_part_of_the_experiment_id() {
    let mut value = spec();
    let id = value.experiment_id.clone();
    value.run_order = vec!["pi".to_string(), "liberado".to_string()];
    assert_eq!(value.compute_experiment_id().unwrap(), id);
}

#[test]
fn alternate_run_order_flips_on_parity() {
    assert_eq!(alternate_run_order(0), vec!["liberado", "pi"]);
    assert_eq!(alternate_run_order(1), vec!["pi", "liberado"]);
    assert_eq!(alternate_run_order(2), vec!["liberado", "pi"]);
}

#[test]
fn four_way_harness_list_is_accepted() {
    let mut value = spec();
    value.harnesses = vec![
        HarnessRequest::new("liberado"),
        HarnessRequest::new("pi"),
        HarnessRequest::new("hermes"),
        HarnessRequest::new("deepagents"),
    ];
    value.run_order = default_four_way_run_order();
    value = value.finalize().unwrap();
    value.validate().unwrap();
}

#[test]
fn unknown_harness_id_is_rejected() {
    let mut value = spec();
    value.harnesses = vec![
        HarnessRequest::new("liberado"),
        HarnessRequest::new("cline"),
    ];
    value.run_order = vec!["liberado".into(), "cline".into()];
    let err = value.finalize().unwrap_err();
    assert!(err.contains("unsupported harness 'cline'"), "{err}");
}

#[test]
fn hermes_without_the_four_way_set_is_rejected() {
    let mut value = spec();
    value.harnesses = vec![
        HarnessRequest::new("liberado"),
        HarnessRequest::new("pi"),
        HarnessRequest::new("hermes"),
    ];
    value.run_order = vec!["liberado".into(), "pi".into(), "hermes".into()];
    let err = value.finalize().unwrap_err();
    assert!(err.contains("four-harness C3 set"), "{err}");
}

#[test]
fn four_way_run_order_rotates_fairly() {
    let ids = default_four_way_run_order();
    assert_eq!(
        rotate_run_order(0, &ids),
        vec!["liberado", "pi", "hermes", "deepagents"]
    );
    assert_eq!(
        rotate_run_order(1, &ids),
        vec!["pi", "hermes", "deepagents", "liberado"]
    );
    assert_eq!(
        rotate_run_order(2, &ids),
        vec!["hermes", "deepagents", "liberado", "pi"]
    );
    assert_eq!(
        rotate_run_order(3, &ids),
        vec!["deepagents", "liberado", "pi", "hermes"]
    );
    assert_eq!(rotate_run_order(4, &ids), ids);
}

#[test]
fn known_harness_ids_are_the_c3_set() {
    assert!(is_known_harness_id("liberado"));
    assert!(is_known_harness_id("pi"));
    assert!(is_known_harness_id("hermes"));
    assert!(is_known_harness_id("deepagents"));
    assert!(!is_known_harness_id("deep-agents"));
    assert!(is_supported_adapter_set(&["liberado", "pi"]));
    assert!(is_supported_adapter_set(&[
        "deepagents",
        "hermes",
        "liberado",
        "pi"
    ]));
    assert!(!is_supported_adapter_set(&["liberado", "pi", "hermes"]));
}
