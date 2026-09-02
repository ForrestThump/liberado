//! Split from `preflight.rs` for module-health boundaries.

use super::*;
use crate::contract::*;
use chrono::Utc;
use liberado_common::process::std_command;
use std::fs;
use std::path::Path;

fn git(repository: &Path, arguments: &[&str]) {
    let status = std_command("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?} failed");
}

#[test]
fn resolved_credentials_never_debug_the_secret() {
    let value = ResolvedCredential("top-secret".to_string());
    let debug = format!("{value:?}");
    assert!(!debug.contains("top-secret"));
    assert!(debug.contains("redacted"));
}

#[test]
fn policy_binds_base_url_and_disables_binary_overrides() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().canonicalize().unwrap();
    let mut spec = JobSpec {
        version: JOB_SPEC_VERSION,
        job_id: JobId::new(),
        submitted_at: Utc::now(),
        repository: repository.clone(),
        base_revision: "deadbeef".to_string(),
        task: TaskBundle::new("task.txt", "test".to_string()).unwrap(),
        harnesses: vec![HarnessRequest {
            id: "liberado".to_string(),
            binary: Some(repository.join("runner.exe")),
            git_sha: None,
        }],
        run_order: vec!["liberado".to_string()],
        model: ModelPins {
            provider: "openrouter".to_string(),
            model: "deepseek/test".to_string(),
            base_url: "https://credential-thief.invalid".to_string(),
            credential_alias: "openrouter-default".to_string(),
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
    let mut policy = WorkerPolicy::for_repository(repository);
    let error = validate_policy(&spec, &policy).unwrap_err();
    assert!(error.to_string().contains("base URL"));

    spec.model.base_url = "https://openrouter.ai/api/v1".to_string();
    spec = spec.finalize().unwrap();
    let error = validate_policy(&spec, &policy).unwrap_err();
    assert!(error.to_string().contains("binary overrides"));

    policy.allow_binary_overrides = true;
    validate_policy(&spec, &policy).unwrap();
}

#[test]
fn require_program_fails_for_unknown_programs() {
    let err = require_program("liberado-harness-eval-definitely-not-a-program").unwrap_err();
    assert!(
        err.to_string()
            .contains("required program is not available"),
        "{err}"
    );
}

#[test]
fn preflight_flags_missing_siblings_index_locks_and_disk_space() {
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
            base_url: "http://127.0.0.1:9".to_string(),
            credential_alias: "openrouter-default".to_string(),
            thinking: "high".to_string(),
            max_turns: 1,
            sampling: SAMPLING_OMITTED.to_string(),
        },
        limits: ResourceLimits {
            compile_timeout_secs: 1,
            run_timeout_secs: 1,
            minimum_free_bytes: 0,
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

    let mut policy = WorkerPolicy::for_repository(repository.clone());
    policy.minimum_free_bytes = 0;
    policy.estimated_build_bytes_per_harness = 0;
    policy.allow_binary_overrides = true;
    policy.base_urls.insert(
        "openrouter".to_string(),
        vec!["http://127.0.0.1:9".to_string()],
    );
    policy.credential_aliases.insert(
        "openrouter-default".to_string(),
        "LIBERADO_PREFLIGHT_E2E_KEY".to_string(),
    );
    unsafe { std::env::set_var("LIBERADO_PREFLIGHT_E2E_KEY", "dummy") };

    // The happy path passes and resolves the credential without printing it.
    let (report, credential) = run(&spec, &policy).unwrap();
    assert_eq!(report.credential_environment, "LIBERADO_PREFLIGHT_E2E_KEY");
    assert_eq!(credential.expose(), "dummy");
    assert!(!format!("{credential:?}").contains("dummy"));

    // Leftover nested clones are optional; cargo fetches the git+tag pins.
    fs::remove_dir_all(repository.join("turbovault")).unwrap();
    fs::remove_dir_all(repository.join("turbomcp")).unwrap();
    run(&spec, &policy).expect("missing leftover clones must not fail preflight");

    // A stale worktree index lock blocks the run rather than corrupting the pinned worktrees.
    let worktree_lock = repository.join(".git/worktrees/some-worktree/index.lock");
    fs::create_dir_all(worktree_lock.parent().unwrap()).unwrap();
    fs::write(&worktree_lock, "").unwrap();
    let err = run(&spec, &policy).unwrap_err();
    assert!(err.to_string().contains("index.lock"), "{err}");
    fs::remove_file(&worktree_lock).unwrap();

    // The disk reserve is enforced against the estimate.
    policy.estimated_build_bytes_per_harness = u64::MAX;
    let err = run(&spec, &policy).unwrap_err();
    assert!(err.to_string().contains("free bytes"), "{err}");

    unsafe { std::env::remove_var("LIBERADO_PREFLIGHT_E2E_KEY") };
}

#[test]
fn validate_policy_rejects_disallowed_provider_model_and_base_url() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(&repository).unwrap();
    let spec = JobSpec {
        version: JOB_SPEC_VERSION,
        job_id: JobId::new(),
        submitted_at: Utc::now(),
        repository: repository.clone(),
        base_revision: "HEAD".to_string(),
        task: TaskBundle::new("task.txt", "test task".to_string()).unwrap(),
        harnesses: vec![HarnessRequest {
            id: "liberado".to_string(),
            binary: None,
            git_sha: None,
        }],
        run_order: vec!["liberado".to_string()],
        model: ModelPins {
            provider: "openrouter".to_string(),
            model: "deepseek/test".to_string(),
            base_url: "http://127.0.0.1:9".to_string(),
            credential_alias: "openrouter-default".to_string(),
            thinking: "high".to_string(),
            max_turns: 1,
            sampling: SAMPLING_OMITTED.to_string(),
        },
        limits: ResourceLimits {
            compile_timeout_secs: 1,
            run_timeout_secs: 1,
            minimum_free_bytes: 0,
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
    let mut policy = WorkerPolicy::for_repository(repository);
    policy.minimum_free_bytes = 0;
    policy.estimated_build_bytes_per_harness = 0;
    policy.allow_binary_overrides = true;
    policy.base_urls.insert(
        "openrouter".to_string(),
        vec!["http://127.0.0.1:9".to_string()],
    );
    policy.credential_aliases.insert(
        "openrouter-default".to_string(),
        "LIBERADO_PREFLIGHT_DENY_KEY".to_string(),
    );
    unsafe { std::env::set_var("LIBERADO_PREFLIGHT_DENY_KEY", "dummy") };

    let mut disallowed_provider = spec.clone();
    disallowed_provider.model.provider = "evil-provider".to_string();
    let err = run(&disallowed_provider.finalize().unwrap(), &policy).unwrap_err();
    assert!(
        err.to_string()
            .contains("provider 'evil-provider' is not allowed"),
        "{err}"
    );

    let mut disallowed_model = spec.clone();
    disallowed_model.model.model = "gpt-4o".to_string();
    let err = run(&disallowed_model.finalize().unwrap(), &policy).unwrap_err();
    assert!(
        err.to_string().contains("model 'gpt-4o' is not allowed"),
        "{err}"
    );

    let mut disallowed_url = spec.clone();
    disallowed_url.model.base_url = "https://evil.example.invalid/v1".to_string();
    let err = run(&disallowed_url.finalize().unwrap(), &policy).unwrap_err();
    assert!(err.to_string().contains("base URL"), "{err}");

    // Over the turn budget is also a policy violation.
    let mut too_many_turns = spec.clone();
    too_many_turns.model.max_turns = 401;
    let err = run(&too_many_turns.finalize().unwrap(), &policy).unwrap_err();
    assert!(err.to_string().contains("exceeds worker policy"), "{err}");

    unsafe { std::env::remove_var("LIBERADO_PREFLIGHT_DENY_KEY") };
}

#[test]
fn missing_external_binary_fails_before_any_paid_work() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("no-such-hermes");
    let err = check_external_binary(
        &HarnessRequest {
            id: "hermes".to_string(),
            binary: Some(missing.clone()),
            git_sha: None,
        },
        "Hermes",
        "hermes",
    )
    .unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
    assert!(
        err.to_string().contains(&missing.display().to_string()),
        "{err}"
    );

    let err = check_external_binary(
        &HarnessRequest {
            id: "deepagents".to_string(),
            binary: Some(temp.path().join("no-such-dcode")),
            git_sha: None,
        },
        "Deep Agents",
        "deepagents",
    )
    .unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
}
