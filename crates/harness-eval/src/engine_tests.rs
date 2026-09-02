//! Split from `engine.rs` for module-health boundaries.

use super::*;
use liberado_common::process::std_command;
use std::path::PathBuf;

#[test]
fn coordinator_accepts_two_way_and_four_way_adapter_sets() {
    assert!(is_supported_adapter_set(&["liberado", "pi"]));
    assert!(is_supported_adapter_set(&[
        "hermes",
        "deepagents",
        "pi",
        "liberado"
    ]));
    assert!(!is_supported_adapter_set(&["liberado"]));
    assert!(!is_supported_adapter_set(&["liberado", "pi", "hermes"]));
}

#[test]
fn missing_harness_results_are_host_infrastructure_failures() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path().join("jobs"));
    let artifact_root = temp.path().join("artifacts");
    let job_id = JobId::new();
    let harnesses = ["liberado", "pi"]
        .into_iter()
        .map(|harness| {
            (
                harness.to_string(),
                HarnessResult {
                    harness: harness.to_string(),
                    exit_code: None,
                    verifier_exit_code: None,
                    head_commit: None,
                    archive_branch: None,
                    accepted: false,
                    diagnostics: Vec::new(),
                    started_at: None,
                    finished_at: None,
                    duration_secs: None,
                    turns_used: None,
                    tokens_in: None,
                    tokens_out: None,
                },
            )
        })
        .collect();
    let run_result: Result<(), Box<dyn Error>> = Err("warm-up failed".into());

    let classification = classify(&run_result, &harnesses, &artifact_root, &store, &job_id);

    assert_eq!(
        classification,
        Some((
            FailureClass::HostInfrastructureFailure,
            "warm-up failed".to_string()
        ))
    );
}

#[test]
fn unpaid_preflight_failure_is_terminal_and_reported() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("turbovault")).unwrap();
    fs::create_dir_all(repository.join("turbomcp")).unwrap();
    fs::write(repository.join("README.md"), "test\n").unwrap();
    git(&repository, &["init"]);
    git(&repository, &["config", "user.email", "test@example.com"]);
    git(&repository, &["config", "user.name", "Test"]);
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "base"]);
    let fake = temp.path().join("fake.exe");
    fs::write(&fake, "not executable").unwrap();
    let spec = JobSpec {
        version: JOB_SPEC_VERSION,
        job_id: JobId::new(),
        submitted_at: Utc::now(),
        repository: repository.clone(),
        base_revision: "HEAD".to_string(),
        task: TaskBundle::new("task.txt", "test task".to_string()).unwrap(),
        harnesses: vec![
            HarnessRequest {
                id: "liberado".to_string(),
                binary: Some(fake.clone()),
                git_sha: None,
            },
            HarnessRequest {
                id: "pi".to_string(),
                binary: Some(fake),
                git_sha: None,
            },
        ],
        run_order: default_run_order(),
        model: ModelPins {
            provider: "openrouter".to_string(),
            model: "deepseek/test".to_string(),
            base_url: "https://example.invalid".to_string(),
            credential_alias: "missing-test".to_string(),
            thinking: "high".to_string(),
            max_turns: 1,
            sampling: SAMPLING_OMITTED.to_string(),
        },
        limits: ResourceLimits {
            compile_timeout_secs: 1,
            run_timeout_secs: 1,
            minimum_free_bytes: 1,
            verifier_repair_attempts: 0,
        },
        verifier: VerifierProfile::WorkspaceTests,
        task_aware_context: false,
        acceptance: None,
        experiment: None,
        experiment_id: String::new(),
    }
    .finalize()
    .unwrap();
    let store = JobStore::for_repository(&repository);
    store
        .create_with_inputs(&spec, |root| {
            fs::write(root.join("input/task.txt"), &spec.task.text)
        })
        .unwrap();
    let mut policy = WorkerPolicy::for_repository(repository);
    policy.minimum_free_bytes = 1;
    // Neutralize the disk-space preflight so it cannot fire before the credential check this
    // test actually targets. The default estimate is 15 GB per harness; a Windows runner with
    // less free space than that estimate fails here with a disk message instead, and this
    // assertion then reports a red that has nothing to do with credentials (seen on main).
    policy.estimated_build_bytes_per_harness = 0;
    policy.maximum_compile_timeout_secs = 1;
    policy.maximum_run_timeout_secs = 1;
    policy.maximum_turns = 1;
    policy.allow_binary_overrides = true;
    policy.base_urls.insert(
        "openrouter".to_string(),
        vec!["https://example.invalid".to_string()],
    );
    policy.credential_aliases.insert(
        "missing-test".to_string(),
        "LIBERADO_TEST_CREDENTIAL_THAT_MUST_NOT_EXIST_92C8".to_string(),
    );
    let report = execute(&store, &spec.job_id, &policy).unwrap();
    assert_eq!(report.status, JobStatus::Failed);
    assert_eq!(
        report.failure_class,
        Some(FailureClass::HostInfrastructureFailure)
    );
    assert!(report.diagnostics[0].contains("credential environment"));
    assert!(!store.job_root(&spec.job_id).join("execution").exists());
}

fn git(repository: &Path, arguments: &[&str]) {
    let status = std_command("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?} failed");
}

fn harness_result(
    harness: &str,
    exit_code: Option<i32>,
    verifier_exit_code: Option<i32>,
) -> HarnessResult {
    HarnessResult {
        harness: harness.to_string(),
        exit_code,
        verifier_exit_code,
        head_commit: Some("abc123".to_string()),
        archive_branch: Some("archive/abc123".to_string()),
        accepted: exit_code == Some(0) && verifier_exit_code == Some(0),
        diagnostics: Vec::new(),
        started_at: None,
        finished_at: None,
        duration_secs: None,
        turns_used: None,
        tokens_in: None,
        tokens_out: None,
    }
}

fn two_harnesses(liberado: HarnessResult, pi: HarnessResult) -> BTreeMap<String, HarnessResult> {
    BTreeMap::from([("liberado".to_string(), liberado), ("pi".to_string(), pi)])
}

#[test]
fn classify_clean_run_is_none() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path().join("jobs"));
    let ok: Result<(), Box<dyn Error>> = Ok(());
    let harnesses = two_harnesses(
        harness_result("liberado", Some(0), Some(0)),
        harness_result("pi", Some(0), Some(0)),
    );
    let classification = classify(&ok, &harnesses, temp.path(), &store, &JobId::new());
    assert_eq!(classification, None);
}

#[test]
fn classify_flags_cancellation_even_with_clean_exits() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path().join("jobs"));
    fs::create_dir_all(store.root()).unwrap();
    let job_id = JobId::new();
    store.create(&spec_fixture(&job_id)).unwrap();
    store.request_cancel(&job_id).unwrap();
    let ok: Result<(), Box<dyn Error>> = Ok(());
    let harnesses = two_harnesses(
        harness_result("liberado", Some(0), Some(0)),
        harness_result("pi", Some(0), Some(0)),
    );
    let classification = classify(&ok, &harnesses, temp.path(), &store, &job_id);
    assert_eq!(
        classification,
        Some((
            FailureClass::Cancelled,
            "comparison was cancelled".to_string()
        ))
    );
}

#[test]
fn classify_flags_timeouts_from_run_or_launch_errors() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path().join("jobs"));
    let run_message = "harness process exceeded its 30 second wall-clock limit and was killed";
    let run_result: Result<(), Box<dyn Error>> = Err(run_message.into());
    let harnesses = two_harnesses(
        harness_result("liberado", None, None),
        harness_result("pi", None, None),
    );
    let classification = classify(&run_result, &harnesses, temp.path(), &store, &JobId::new());
    assert_eq!(
        classification,
        Some((FailureClass::Timeout, run_message.to_string()))
    );

    // A launch-error.txt mentioning the wall-clock limit wins even when the run error does not.
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir_all(artifact_root.join("pi")).unwrap();
    fs::write(
        artifact_root.join("pi/launch-error.txt"),
        "comparison hit the wall-clock limit",
    )
    .unwrap();
    let other: Result<(), Box<dyn Error>> = Err("something else".into());
    let classification = classify(&other, &harnesses, &artifact_root, &store, &JobId::new());
    assert_eq!(
        classification,
        Some((
            FailureClass::Timeout,
            "comparison hit the wall-clock limit".to_string()
        ))
    );
}

#[test]
fn classify_harness_and_verifier_failures() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path().join("jobs"));
    let ok: Result<(), Box<dyn Error>> = Ok(());

    let harnesses = two_harnesses(
        harness_result("liberado", Some(1), Some(0)),
        harness_result("pi", Some(0), Some(0)),
    );
    let classification = classify(&ok, &harnesses, temp.path(), &store, &JobId::new());
    assert_eq!(
        classification,
        Some((
            FailureClass::HarnessFailure,
            "one or more harnesses failed".to_string()
        ))
    );

    // A failing common verifier is distinct from a failing harness.
    let harnesses = two_harnesses(
        harness_result("liberado", Some(0), Some(1)),
        harness_result("pi", Some(0), Some(0)),
    );
    let classification = classify(&ok, &harnesses, temp.path(), &store, &JobId::new());
    assert_eq!(
        classification,
        Some((
            FailureClass::VerifierFailure,
            "one or more common verifiers failed".to_string()
        ))
    );

    // When the adapters and verifiers all pass, a leftover run error is a task failure.
    let failing: Result<(), Box<dyn Error>> = Err("model returned a refusal".into());
    let harnesses = two_harnesses(
        harness_result("liberado", Some(0), Some(0)),
        harness_result("pi", Some(0), Some(0)),
    );
    let classification = classify(&failing, &harnesses, temp.path(), &store, &JobId::new());
    assert_eq!(
        classification,
        Some((
            FailureClass::TaskFailure,
            "model returned a refusal".to_string()
        ))
    );
}

#[test]
fn collect_results_parses_saved_results_and_flags_missing() {
    let temp = tempfile::tempdir().unwrap();
    let artifact_root = temp.path().join("artifacts/harnesses");
    fs::create_dir_all(artifact_root.join("liberado")).unwrap();
    fs::create_dir_all(artifact_root.join("pi")).unwrap();
    fs::write(
        artifact_root.join("liberado/result.json"),
        serde_json::json!({
            "harness": "liberado",
            "base_commit": "abc123",
            "head_commit": "def456",
            "archive_branch": "archive/def456",
            "exit_code": 0,
            "verifier_exit_code": 0,
            "session_id": "run-liberado",
            "saved_at": "2026-08-01T00:00:00Z",
            "had_uncommitted_changes": false,
        })
        .to_string(),
    )
    .unwrap();

    let spec = JobSpec {
        version: JOB_SPEC_VERSION,
        job_id: JobId::new(),
        submitted_at: Utc::now(),
        repository: PathBuf::from("C:/repo"),
        base_revision: "main".to_string(),
        task: TaskBundle::new("task.txt", "do it".to_string()).unwrap(),
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
            base_url: "https://example.invalid".to_string(),
            credential_alias: "openrouter-default".to_string(),
            thinking: "high".to_string(),
            max_turns: 1,
            sampling: SAMPLING_OMITTED.to_string(),
        },
        limits: ResourceLimits::default(),
        verifier: VerifierProfile::WorkspaceTests,
        task_aware_context: false,
        acceptance: None,
        experiment: None,
        experiment_id: String::new(),
    }
    .finalize()
    .unwrap();

    let results = collect_results(&spec, &artifact_root).unwrap();
    let liberado = &results["liberado"];
    assert_eq!(liberado.exit_code, Some(0));
    assert_eq!(liberado.verifier_exit_code, Some(0));
    assert!(liberado.accepted);
    assert_eq!(liberado.head_commit.as_deref(), Some("def456"));
    // The pi harness has no result.json: reported as missing rather than dropped.
    let pi = &results["pi"];
    assert_eq!(pi.exit_code, None);
    assert!(
        pi.diagnostics
            .contains(&"result.json is missing".to_string())
    );

    // A result whose harness field disagrees with its directory is an error.
    let bad = artifact_root.join("liberado/result.json");
    let text = fs::read_to_string(&bad).unwrap().replace("liberado", "pi");
    fs::write(&bad, text).unwrap();
    let err = collect_results(&spec, &artifact_root).unwrap_err();
    assert!(
        err.to_string()
            .contains("does not match artifact directory"),
        "{err}"
    );
}

#[test]
fn execute_returns_cancelled_before_any_paid_work() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path().join("jobs"));
    fs::create_dir_all(store.root()).unwrap();
    let job_id = JobId::new();
    store.create(&spec_fixture(&job_id)).unwrap();
    store.request_cancel(&job_id).unwrap();
    let policy = WorkerPolicy::for_repository(temp.path().to_path_buf());
    let report = execute(&store, &job_id, &policy).unwrap();
    assert_eq!(report.status, JobStatus::Cancelled);
    assert_eq!(report.failure_class, Some(FailureClass::Cancelled));
    assert_eq!(
        store.load_state(&job_id).unwrap().status,
        JobStatus::Cancelled
    );
}

fn spec_fixture(job_id: &JobId) -> JobSpec {
    JobSpec {
        version: JOB_SPEC_VERSION,
        job_id: job_id.clone(),
        submitted_at: Utc::now(),
        repository: PathBuf::from("C:/repo"),
        base_revision: "main".to_string(),
        task: TaskBundle::new("task.txt", "do it".to_string()).unwrap(),
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
            base_url: "https://example.invalid".to_string(),
            credential_alias: "openrouter-default".to_string(),
            thinking: "high".to_string(),
            max_turns: 1,
            sampling: SAMPLING_OMITTED.to_string(),
        },
        limits: ResourceLimits::default(),
        verifier: VerifierProfile::WorkspaceTests,
        task_aware_context: false,
        acceptance: None,
        experiment: None,
        experiment_id: String::new(),
    }
    .finalize()
    .unwrap()
}
