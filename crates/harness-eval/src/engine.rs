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

    let mut ids = harness_ids(&spec);
    ids.sort();
    if ids != ["liberado", "pi"] {
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
    if let Err(error) = legacy::prepare_parsed(
        &execution_root,
        &preflight.repository,
        &preflight.base_commit,
        &preflight.base_commit,
        spec.limits.compile_timeout_secs,
    ) {
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
    let run_args = legacy::run_args_from_spec(
        &spec,
        &job_root,
        &execution_root,
        &preflight.credential_environment,
    );
    let run_result = legacy::run_parsed(run_args, credential);

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
                run_order: spec.run_order.clone(),
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

fn collect_results(
    spec: &JobSpec,
    artifact_root: &Path,
) -> Result<BTreeMap<String, HarnessResult>, Box<dyn Error>> {
    let mut results = BTreeMap::new();
    for harness in &spec.harnesses {
        let harness_dir = artifact_root.join(&harness.id);
        let metrics = crate::metrics::HarnessMetrics::collect(&harness.id, &harness_dir);
        let path = harness_dir.join("result.json");
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
                    started_at: metrics.started_at,
                    finished_at: metrics.finished_at,
                    duration_secs: metrics.duration_secs,
                    turns_used: metrics.turns_used,
                    tokens_in: metrics.tokens_in,
                    tokens_out: metrics.tokens_out,
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
                started_at: metrics.started_at,
                finished_at: metrics.finished_at,
                duration_secs: metrics.duration_secs,
                turns_used: metrics.turns_used,
                tokens_in: metrics.tokens_in,
                tokens_out: metrics.tokens_out,
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
    // A missing exit code means the adapter never produced a result. Treat that as a host
    // infrastructure failure before checking ordinary non-zero harness exits.
    if harnesses.values().all(|result| result.exit_code.is_none())
        && let Some(message) = run_error.clone()
    {
        return Some((FailureClass::HostInfrastructureFailure, message));
    }
    if harnesses.values().any(|result| result.exit_code != Some(0)) {
        return Some((
            FailureClass::HarnessFailure,
            run_error.unwrap_or_else(|| "one or more harnesses failed".to_string()),
        ));
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
        run_order: spec.run_order.clone(),
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
                },
                HarnessRequest {
                    id: "pi".to_string(),
                    binary: Some(fake),
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
}
