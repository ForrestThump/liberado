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

/// Shared execution context threaded through every stage: the store handle, the job being
/// driven, and the progress tracker. Stages that finish the job early return the report;
/// stages that need data hand it back via `Stage::Continue`.
struct RunContext<'a> {
    store: &'a JobStore,
    job_id: &'a JobId,
    spec: &'a JobSpec,
    started_at: DateTime<Utc>,
    tracker: &'a mut StateTracker<'a>,
}

/// Outcome of one `execute` stage: either produce a value to continue with, or the job
/// finished (a failure report was already persisted and the caller must return it).
enum Stage<T> {
    Continue(T),
    Finished(Box<ComparisonReport>),
}

/// Preserved per-harness results plus any cleanup diagnostics collected on the way out.
struct PreservedResults {
    harnesses: BTreeMap<String, HarnessResult>,
    cleanup_diagnostics: Vec<String>,
}

/// Drive one comparison job to a terminal report. Stages are extracted so each decision
/// boundary (input validation, preflight, preparation, preservation) is a small function;
/// `execute` only sequences them and returns whatever stage finished the job.
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

    let mut ctx = RunContext {
        store,
        job_id,
        spec: &spec,
        started_at,
        tracker: &mut tracker,
    };

    if let Stage::Finished(report) = verify_inputs(&mut ctx)? {
        return Ok(*report);
    }

    let (preflight, credential) = match run_preflight(&mut ctx, policy)? {
        Stage::Continue(ready) => ready,
        Stage::Finished(report) => return Ok(*report),
    };

    let job_root = ctx.store.job_root(ctx.job_id);
    let execution_root = job_root.join("execution");
    if let Stage::Finished(report) = prepare_worktrees(&mut ctx, &preflight, &execution_root)? {
        return Ok(*report);
    }

    ctx.tracker.advance(
        JobStatus::Running,
        "run",
        "running harness adapters in declared order",
    )?;
    let run_args = legacy::run_args_from_spec(
        ctx.spec,
        &job_root,
        &execution_root,
        &preflight.credential_environment,
    );
    let run_result = legacy::run_parsed(run_args, credential);

    ctx.tracker.advance(
        JobStatus::Verifying,
        "verify",
        "common verification finished; classifying results",
    )?;
    ctx.tracker.advance(
        JobStatus::Preserving,
        "preserve",
        "normalizing durable artifacts and results",
    )?;
    let preserved = preserve_and_collect(&ctx, &job_root, &execution_root, policy)?;

    finish_and_report(&mut ctx, &preflight.base_commit, run_result, preserved)
}

/// Stage 1: validate that every captured input the job needs exists before anything runs.
fn verify_inputs(ctx: &mut RunContext<'_>) -> Result<Stage<()>, Box<dyn Error>> {
    if let Err(error) =
        crate::transport::verify_captured_inputs(ctx.spec, &ctx.store.job_root(ctx.job_id))
    {
        return Ok(Stage::Finished(Box::new(finish_failure(
            ctx.store,
            ctx.spec,
            ctx.started_at,
            ctx.tracker,
            FailureClass::HostInfrastructureFailure,
            format!("captured input validation failed: {error}"),
            BTreeMap::new(),
            None,
        )?)));
    }
    Ok(Stage::Continue(()))
}

/// Stage 2: run preflight, persist its output alongside the experiment spec, and resolve
/// the credential.
fn run_preflight(
    ctx: &mut RunContext<'_>,
    policy: &WorkerPolicy,
) -> Result<
    Stage<(
        crate::preflight::PreflightReport,
        crate::preflight::ResolvedCredential,
    )>,
    Box<dyn Error>,
> {
    ctx.tracker.advance(
        JobStatus::Preflight,
        "preflight",
        "checking all unpaid prerequisites",
    )?;
    let (preflight, credential) = match preflight::run(ctx.spec, policy) {
        Ok(result) => result,
        Err(error) => {
            return Ok(Stage::Finished(Box::new(finish_failure(
                ctx.store,
                ctx.spec,
                ctx.started_at,
                ctx.tracker,
                FailureClass::HostInfrastructureFailure,
                format!("preflight failed: {error}"),
                BTreeMap::new(),
                None,
            )?)));
        }
    };
    atomic_json(
        &ctx.store.job_root(ctx.job_id).join("preflight.json"),
        &serde_json::json!({
            "repository": preflight.repository,
            "base_commit": preflight.base_commit,
            "free_bytes": preflight.free_bytes,
            "estimated_required_bytes": preflight.estimated_required_bytes,
            "credential_alias": ctx.spec.model.credential_alias,
            "credential_environment": preflight.credential_environment,
            "checked_at": Utc::now(),
        }),
    )?;
    atomic_json(
        &ctx.store.job_root(ctx.job_id).join("experiment.json"),
        ctx.spec,
    )?;
    Ok(Stage::Continue((preflight, credential)))
}

/// Stage 3: pin the adapter set and create the isolated worktrees the adapters run in.
fn prepare_worktrees(
    ctx: &mut RunContext<'_>,
    preflight: &crate::preflight::PreflightReport,
    execution_root: &Path,
) -> Result<Stage<()>, Box<dyn Error>> {
    let mut ids = harness_ids(ctx.spec);
    ids.sort();
    if !is_supported_adapter_set(&ids) {
        return Ok(Stage::Finished(Box::new(finish_failure(
            ctx.store,
            ctx.spec,
            ctx.started_at,
            ctx.tracker,
            FailureClass::HostInfrastructureFailure,
            "the v1 coordinator requires the liberado and pi adapters, or the four-harness C3 set (liberado, pi, hermes, deepagents)".to_string(),
            BTreeMap::new(),
            Some(preflight.base_commit.clone()),
        )?)));
    }

    ctx.tracker.advance(
        JobStatus::Preparing,
        "prepare",
        "creating pinned isolated worktrees",
    )?;
    if let Err(error) = legacy::prepare_parsed(
        execution_root,
        &preflight.repository,
        &preflight.base_commit,
        &preflight.base_commit,
        ctx.spec.limits.compile_timeout_secs,
        &ids,
    ) {
        return Ok(Stage::Finished(Box::new(finish_failure(
            ctx.store,
            ctx.spec,
            ctx.started_at,
            ctx.tracker,
            FailureClass::HostInfrastructureFailure,
            format!("comparison preparation failed: {error}"),
            BTreeMap::new(),
            Some(preflight.base_commit.clone()),
        )?)));
    }
    Ok(Stage::Continue(()))
}

/// Stage 4: normalize durable harness artifacts, clean disposable build state per policy,
/// and collect the per-harness results.
fn preserve_and_collect(
    ctx: &RunContext<'_>,
    job_root: &Path,
    execution_root: &Path,
    policy: &WorkerPolicy,
) -> Result<PreservedResults, Box<dyn Error>> {
    let normalized_root = job_root.join("artifacts/harnesses");
    fs::create_dir_all(&normalized_root)?;
    for harness in &ctx.spec.harnesses {
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
        && let Err(error) = legacy::remove_job_worktrees(execution_root)
    {
        cleanup_diagnostics.push(format!("could not remove completed worktrees: {error}"));
    }
    let harnesses = collect_results(ctx.spec, &normalized_root)?;
    Ok(PreservedResults {
        harnesses,
        cleanup_diagnostics,
    })
}

/// Stage 5: classify the run, persist the terminal report (report before state, so a reader
/// that observes the terminal state can always read report.json), and return it.
fn finish_and_report(
    ctx: &mut RunContext<'_>,
    base_commit: &str,
    run_result: Result<(), Box<dyn Error>>,
    preserved: PreservedResults,
) -> Result<ComparisonReport, Box<dyn Error>> {
    let job_root = ctx.store.job_root(ctx.job_id);
    let normalized_root = job_root.join("artifacts/harnesses");
    let classification = classify(
        &run_result,
        &preserved.harnesses,
        &normalized_root,
        ctx.store,
        ctx.job_id,
    );
    match classification {
        None => {
            let report = ComparisonReport {
                version: 1,
                job_id: ctx.job_id.clone(),
                experiment_id: ctx.spec.experiment_id.clone(),
                status: JobStatus::Succeeded,
                failure_class: None,
                base_commit: Some(base_commit.to_string()),
                started_at: ctx.started_at,
                finished_at: Utc::now(),
                harnesses: preserved.harnesses,
                run_order: ctx.spec.run_order.clone(),
                diagnostics: preserved.cleanup_diagnostics,
                artifact_root: job_root.join("artifacts"),
            };
            // Report before terminal state: a reader that observes the terminal state must be
            // able to read report.json (the state file is what awaiters watch; the report is
            // what they read next). The old order — state then report — let a fast consumer
            // (await_terminal + load_report) see the terminal state before the report landed.
            write_report_or_mark_host_failure(ctx.store, ctx.tracker, &report)?;
            ctx.tracker.advance(
                JobStatus::Succeeded,
                "complete",
                "all harness results were preserved",
            )?;
            Ok(report)
        }
        Some((class, mut message)) => {
            if !preserved.cleanup_diagnostics.is_empty() {
                message.push_str("; ");
                message.push_str(&preserved.cleanup_diagnostics.join("; "));
            }
            finish_failure(
                ctx.store,
                ctx.spec,
                ctx.started_at,
                ctx.tracker,
                class,
                message,
                preserved.harnesses,
                Some(base_commit.to_string()),
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
        diagnostics: vec![message.clone()],
        artifact_root: store.job_root(&spec.job_id).join("artifacts"),
    };
    // Report before terminal state — see the success path for why the order is load-bearing.
    write_report_or_mark_host_failure(store, tracker, &report)?;
    tracker.fail(status, class, &message)?;
    Ok(report)
}

/// Write the terminal report. A report-write failure still marks the job terminal — best effort,
/// classified as a host failure — so awaiters return instead of hanging forever, and the error
/// propagates to the caller. The state flip itself stays in the caller so each terminal path
/// keeps its own phase/message wording.
fn write_report_or_mark_host_failure(
    store: &JobStore,
    tracker: &mut StateTracker<'_>,
    report: &ComparisonReport,
) -> Result<(), Box<dyn Error>> {
    if let Err(error) = store.write_report(report) {
        let _ = tracker.fail(
            JobStatus::Failed,
            FailureClass::HostInfrastructureFailure,
            &format!("failed to write report.json: {error}"),
        );
        return Err(error.into());
    }
    Ok(())
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

    fn two_harnesses(
        liberado: HarnessResult,
        pi: HarnessResult,
    ) -> BTreeMap<String, HarnessResult> {
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
}
