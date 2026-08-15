//! Reusable comparison coordinator.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::contract::*;
use crate::journal::{JobStore, atomic_json};
use crate::{legacy, preflight};

pub fn execute(
    store: &JobStore,
    job_id: &JobId,
    policy: &WorkerPolicy,
) -> Result<ComparisonReport, Box<dyn Error>> {
    let _lease = store.acquire_lease(job_id)?;
    let spec = store.load_spec(job_id)?;
    let started_at = Utc::now();
    let mut tracker = StateTracker::new(store, job_id.clone())?;
    if store.cancellation_requested(job_id) {
        return finish_cancelled(store, &spec, started_at, &mut tracker);
    }

    if let Err(error) = crate::transport::verify_captured_inputs(&spec, &store.job_root(job_id)) {
        return finish_failure(
            store,
            &spec,
            started_at,
            &mut tracker,
            FailureClass::HostInfrastructureFailure,
            format!("captured input validation failed: {error}"),
            BTreeMap::new(),
            None,
        );
    }

    tracker.advance(
        JobStatus::Preflight,
        "preflight",
        "checking all unpaid prerequisites",
    )?;
    let (preflight, credential) = match preflight::run(&spec, policy) {
        Ok(result) => result,
        Err(error) => {
            return finish_failure(
                store,
                &spec,
                started_at,
                &mut tracker,
                FailureClass::HostInfrastructureFailure,
                format!("preflight failed: {error}"),
                BTreeMap::new(),
                None,
            );
        }
    };
    atomic_json(
        &store.job_root(job_id).join("preflight.json"),
        &serde_json::json!({
            "repository": preflight.repository,
            "base_commit": preflight.base_commit,
            "free_bytes": preflight.free_bytes,
            "estimated_required_bytes": preflight.estimated_required_bytes,
            "credential_alias": spec.model.credential_alias,
            "credential_environment": preflight.credential_environment,
            "checked_at": Utc::now(),
        }),
    )?;
    atomic_json(&store.job_root(job_id).join("experiment.json"), &spec)?;

    if harness_ids(&spec) != ["liberado", "pi"] {
        return finish_failure(
            store,
            &spec,
            started_at,
            &mut tracker,
            FailureClass::HostInfrastructureFailure,
            "the v1 coordinator requires the liberado and pi adapters".to_string(),
            BTreeMap::new(),
            Some(preflight.base_commit),
        );
    }

    tracker.advance(
        JobStatus::Preparing,
        "prepare",
        "creating pinned isolated worktrees",
    )?;
    let job_root = store.job_root(job_id);
    let execution_root = job_root.join("execution");
    let prepare_args = vec![
        execution_root.to_string_lossy().into_owned(),
        "--source".to_string(),
        spec.repository.to_string_lossy().into_owned(),
        "--commit".to_string(),
        preflight.base_commit.clone(),
        "--compile-timeout-secs".to_string(),
        spec.limits.compile_timeout_secs.to_string(),
    ];
    if let Err(error) = legacy::prepare(&prepare_args) {
        return finish_failure(
            store,
            &spec,
            started_at,
            &mut tracker,
            FailureClass::HostInfrastructureFailure,
            format!("comparison preparation failed: {error}"),
            BTreeMap::new(),
            Some(preflight.base_commit),
        );
    }

    tracker.advance(
        JobStatus::Running,
        "run",
        "running harness adapters in declared order",
    )?;
    let run_args = legacy_run_args(
        &spec,
        &job_root,
        &execution_root,
        &preflight.credential_environment,
    );
    let run_result = legacy::run_with_credential(&run_args, credential);

    tracker.advance(
        JobStatus::Verifying,
        "verify",
        "common verification finished; classifying results",
    )?;
    tracker.advance(
        JobStatus::Preserving,
        "preserve",
        "normalizing durable artifacts and results",
    )?;
    let normalized_root = job_root.join("artifacts/harnesses");
    fs::create_dir_all(&normalized_root)?;
    for harness in &spec.harnesses {
        let source = execution_root.join("artifacts").join(&harness.id);
        let destination = normalized_root.join(&harness.id);
        if source.is_dir() && !destination.exists() {
            fs::rename(source, destination)?;
        }
    }
    let mut cleanup_diagnostics = Vec::new();
    if !policy.retain_build_caches {
        let targets = execution_root.join("targets");
        if targets.is_dir()
            && let Err(error) = fs::remove_dir_all(&targets)
        {
            cleanup_diagnostics.push(format!("could not remove build caches: {error}"));
        }
    }
    if !policy.retain_worktrees
        && let Err(error) = legacy::remove_job_worktrees(&execution_root)
    {
        cleanup_diagnostics.push(format!("could not remove completed worktrees: {error}"));
    }
    let harnesses = collect_results(&spec, &normalized_root)?;
    let classification = classify(&run_result, &harnesses, &normalized_root, store, job_id);
    match classification {
        None => {
            tracker.advance(
                JobStatus::Succeeded,
                "complete",
                "all harness results were preserved",
            )?;
            let report = ComparisonReport {
                version: 1,
                job_id: job_id.clone(),
                experiment_id: spec.experiment_id.clone(),
                status: JobStatus::Succeeded,
                failure_class: None,
                base_commit: Some(preflight.base_commit),
                started_at,
                finished_at: Utc::now(),
                harnesses,
                diagnostics: cleanup_diagnostics,
                artifact_root: job_root.join("artifacts"),
            };
            store.write_report(&report)?;
            Ok(report)
        }
        Some((class, mut message)) => {
            if !cleanup_diagnostics.is_empty() {
                message.push_str("; ");
                message.push_str(&cleanup_diagnostics.join("; "));
            }
            finish_failure(
                store,
                &spec,
                started_at,
                &mut tracker,
                class,
                message,
                harnesses,
                Some(preflight.base_commit),
            )
        }
    }
}

fn legacy_run_args(
    spec: &JobSpec,
    job_root: &Path,
    execution_root: &Path,
    credential_environment: &str,
) -> Vec<String> {
    let mut args = vec![
        execution_root.to_string_lossy().into_owned(),
        "--task".to_string(),
        job_root
            .join("input/task.txt")
            .to_string_lossy()
            .into_owned(),
        "--model".to_string(),
        spec.model.model.clone(),
        "--provider".to_string(),
        spec.model.provider.clone(),
        "--base-url".to_string(),
        spec.model.base_url.clone(),
        "--api-key-env".to_string(),
        credential_environment.to_string(),
        "--thinking".to_string(),
        spec.model.thinking.clone(),
        "--max-turns".to_string(),
        spec.model.max_turns.to_string(),
        "--run-timeout-secs".to_string(),
        spec.limits.run_timeout_secs.to_string(),
        "--verifier-repair-attempts".to_string(),
        spec.limits.verifier_repair_attempts.to_string(),
        "--cancel-file".to_string(),
        job_root
            .join("cancel-requested")
            .to_string_lossy()
            .into_owned(),
    ];
    if spec.task_aware_context {
        args.push("--task-aware-context".to_string());
    }
    for pattern in &spec.write_scope.allow {
        args.extend(["--allow-change".to_string(), pattern.clone()]);
    }
    for pattern in &spec.write_scope.deny {
        args.extend(["--deny-change".to_string(), pattern.clone()]);
    }
    if let Some(acceptance) = &spec.acceptance {
        args.extend([
            "--acceptance-overlay".to_string(),
            job_root
                .join(&acceptance.directory)
                .to_string_lossy()
                .into_owned(),
        ]);
    }
    for harness in &spec.harnesses {
        if let Some(binary) = &harness.binary {
            args.extend([
                format!("--{}-bin", harness.id),
                binary.to_string_lossy().into_owned(),
            ]);
        }
    }
    args
}

fn collect_results(
    spec: &JobSpec,
    artifact_root: &Path,
) -> Result<BTreeMap<String, HarnessResult>, Box<dyn Error>> {
    let mut results = BTreeMap::new();
    for harness in &spec.harnesses {
        let path = artifact_root.join(&harness.id).join("result.json");
        if !path.is_file() {
            results.insert(
                harness.id.clone(),
                HarnessResult {
                    harness: harness.id.clone(),
                    exit_code: None,
                    verifier_exit_code: None,
                    head_commit: None,
                    archive_branch: None,
                    accepted: false,
                    diagnostics: vec!["result.json is missing".to_string()],
                },
            );
            continue;
        }
        let value: LegacySavedResult = serde_json::from_slice(&fs::read(path)?)?;
        if value.harness != harness.id {
            return Err(format!(
                "result harness '{}' does not match artifact directory '{}'",
                value.harness, harness.id
            )
            .into());
        }
        let exit_code = value.exit_code;
        let verifier_exit_code = value.verifier_exit_code;
        results.insert(
            harness.id.clone(),
            HarnessResult {
                harness: harness.id.clone(),
                exit_code,
                verifier_exit_code,
                head_commit: Some(value.head_commit),
                archive_branch: Some(value.archive_branch),
                accepted: exit_code == Some(0) && verifier_exit_code == Some(0),
                diagnostics: Vec::new(),
            },
        );
    }
    Ok(results)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct LegacySavedResult {
    harness: String,
    base_commit: String,
    head_commit: String,
    archive_branch: String,
    exit_code: Option<i32>,
    verifier_exit_code: Option<i32>,
    session_id: Option<String>,
    saved_at: DateTime<Utc>,
    had_uncommitted_changes: bool,
}

fn classify(
    run_result: &Result<(), Box<dyn Error>>,
    harnesses: &BTreeMap<String, HarnessResult>,
    artifact_root: &Path,
    store: &JobStore,
    job_id: &JobId,
) -> Option<(FailureClass, String)> {
    if store.cancellation_requested(job_id) {
        return Some((
            FailureClass::Cancelled,
            "comparison was cancelled".to_string(),
        ));
    }
    let run_error = run_result.as_ref().err().map(ToString::to_string);
    let launch_errors = harnesses
        .keys()
        .filter_map(|harness| {
            fs::read_to_string(artifact_root.join(harness).join("launch-error.txt")).ok()
        })
        .collect::<Vec<_>>();
    if run_error
        .as_deref()
        .is_some_and(|message| message.contains("wall-clock limit"))
        || launch_errors
            .iter()
            .any(|message| message.contains("wall-clock limit"))
    {
        return Some((
            FailureClass::Timeout,
            launch_errors
                .into_iter()
                .find(|message| message.contains("wall-clock limit"))
                .or(run_error)
                .unwrap_or_else(|| "harness timed out".to_string()),
        ));
    }
    for result in harnesses.values() {
        let verifier_stderr = fs::read_to_string(
            artifact_root
                .join(&result.harness)
                .join("verifier.stderr.log"),
        )
        .unwrap_or_default();
        if verifier_stderr.contains("outside the dispatch write scope") {
            return Some((
                FailureClass::ScopeViolation,
                verifier_stderr.trim().to_string(),
            ));
        }
    }
    if harnesses.values().any(|result| result.exit_code != Some(0)) {
        return Some((
            FailureClass::HarnessFailure,
            run_error.unwrap_or_else(|| "one or more harnesses failed".to_string()),
        ));
    }
    if harnesses.values().all(|result| result.exit_code.is_none())
        && let Some(message) = run_error
    {
        return Some((FailureClass::HostInfrastructureFailure, message));
    }
    if harnesses
        .values()
        .any(|result| result.verifier_exit_code != Some(0))
    {
        return Some((
            FailureClass::VerifierFailure,
            run_error.unwrap_or_else(|| "one or more common verifiers failed".to_string()),
        ));
    }
    run_error.map(|message| (FailureClass::TaskFailure, message))
}

fn harness_ids(spec: &JobSpec) -> Vec<&str> {
    spec.harnesses
        .iter()
        .map(|harness| harness.id.as_str())
        .collect()
}

fn finish_cancelled(
    store: &JobStore,
    spec: &JobSpec,
    started_at: DateTime<Utc>,
    tracker: &mut StateTracker<'_>,
) -> Result<ComparisonReport, Box<dyn Error>> {
    finish_failure(
        store,
        spec,
        started_at,
        tracker,
        FailureClass::Cancelled,
        "comparison was cancelled before execution".to_string(),
        BTreeMap::new(),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_failure(
    store: &JobStore,
    spec: &JobSpec,
    started_at: DateTime<Utc>,
    tracker: &mut StateTracker<'_>,
    class: FailureClass,
    message: String,
    harnesses: BTreeMap<String, HarnessResult>,
    base_commit: Option<String>,
) -> Result<ComparisonReport, Box<dyn Error>> {
    let status = if class == FailureClass::Cancelled {
        JobStatus::Cancelled
    } else {
        JobStatus::Failed
    };
    tracker.fail(status, class, &message)?;
    let report = ComparisonReport {
        version: 1,
        job_id: spec.job_id.clone(),
        experiment_id: spec.experiment_id.clone(),
        status,
        failure_class: Some(class),
        base_commit,
        started_at,
        finished_at: Utc::now(),
        harnesses,
        diagnostics: vec![message],
        artifact_root: store.job_root(&spec.job_id).join("artifacts"),
    };
    store.write_report(&report)?;
    Ok(report)
}

struct StateTracker<'a> {
    store: &'a JobStore,
    state: JobState,
}

impl<'a> StateTracker<'a> {
    fn new(store: &'a JobStore, job_id: JobId) -> io::Result<Self> {
        Ok(Self {
            store,
            state: store.load_state(&job_id)?,
        })
    }

    fn advance(&mut self, status: JobStatus, phase: &str, message: &str) -> io::Result<()> {
        self.state.revision += 1;
        self.state.status = status;
        self.state.phase = phase.to_string();
        self.state.updated_at = Utc::now();
        self.state.failure_class = None;
        self.state.message = Some(message.to_string());
        self.store.write_state(&self.state)?;
        self.store.append_job_event(
            &self.state.job_id,
            &JobEvent {
                sequence: self.state.revision,
                at: self.state.updated_at,
                status,
                phase: phase.to_string(),
                message: message.to_string(),
            },
        )
    }

    fn fail(&mut self, status: JobStatus, class: FailureClass, message: &str) -> io::Result<()> {
        self.state.revision += 1;
        self.state.status = status;
        self.state.phase = "terminal".to_string();
        self.state.updated_at = Utc::now();
        self.state.failure_class = Some(class);
        self.state.message = Some(message.to_string());
        self.store.write_state(&self.state)?;
        self.store.append_job_event(
            &self.state.job_id,
            &JobEvent {
                sequence: self.state.revision,
                at: self.state.updated_at,
                status,
                phase: "terminal".to_string(),
                message: message.to_string(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::process::std_command;

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
                },
                HarnessRequest {
                    id: "pi".to_string(),
                    binary: Some(fake),
                },
            ],
            model: ModelPins {
                provider: "openrouter".to_string(),
                model: "deepseek/test".to_string(),
                base_url: "https://example.invalid".to_string(),
                credential_alias: "missing-test".to_string(),
                thinking: "high".to_string(),
                max_turns: 1,
            },
            limits: ResourceLimits {
                compile_timeout_secs: 1,
                run_timeout_secs: 1,
                minimum_free_bytes: 1,
                verifier_repair_attempts: 0,
            },
            verifier: VerifierProfile::WorkspaceTests,
            task_aware_context: false,
            write_scope: WriteScope::default(),
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
}
