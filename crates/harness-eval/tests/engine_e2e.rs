//! Full pipeline tests: queue a job through `transport::submit`, drive it through `engine::execute`
//! (and through the detached worker binary) against a scratch git repository, and land on a
//! terminal report.
//!
//! Harness binaries are either this crate's own worker binary with unrelated arguments (fails
//! fast, no network) or a rustc-compiled exit-0 helper (succeeds). The repository is a committed
//! zero-dependency crate so warm-up (`cargo check --workspace --locked`) and the verifier
//! (`cargo test`) complete in seconds.

use liberado_harness_eval::contract::*;
use liberado_harness_eval::journal::JobStore;
use liberado_harness_eval::{engine, transport};
use std::fs;
use std::path::Path;
use std::process::Command;

const TINY_MANIFEST: &str = "\
[package]
name = \"tiny\"
version = \"0.1.0\"
edition = \"2021\"
";

fn git(repository: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .status()
        .unwrap()
        .success()
}

/// A scratch git repository with a committed zero-dependency crate, optional leftover
/// nested clones, and a committed lockfile so `--locked` accepts the worktrees.
fn scratch_repository(temp: &tempfile::TempDir) -> std::path::PathBuf {
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(repository.join("Cargo.toml"), TINY_MANIFEST).unwrap();
    fs::write(repository.join("src/lib.rs"), "").unwrap();
    for sibling in ["turbovault", "turbomcp"] {
        fs::create_dir(repository.join(sibling)).unwrap();
        fs::write(repository.join(sibling).join("README.md"), sibling).unwrap();
    }
    assert!(
        Command::new("cargo")
            .arg("generate-lockfile")
            .current_dir(&repository)
            .status()
            .unwrap()
            .success()
    );
    assert!(git(&repository, &["init"]));
    assert!(git(&repository, &["add", "."]));
    assert!(git(
        &repository,
        &[
            "-c",
            "user.name=Liberado Test",
            "-c",
            "user.email=liberado@example.invalid",
            "commit",
            "-m",
            "base",
        ]
    ));
    repository
}

fn permissive_policy(repository: &Path, credential_env: &str) -> WorkerPolicy {
    let mut policy = WorkerPolicy::for_repository(repository.to_path_buf());
    policy.minimum_free_bytes = 0;
    policy.estimated_build_bytes_per_harness = 0;
    policy.allow_binary_overrides = true;
    policy.base_urls.insert(
        "openrouter".to_string(),
        vec!["http://127.0.0.1:9".to_string()],
    );
    // Each test uses its own variable name: the e2e tests run concurrently in one process and the
    // executor resolves credentials from the process environment.
    policy
        .credential_aliases
        .insert("openrouter-default".to_string(), credential_env.to_string());
    policy
}

/// Compile a tiny executable that exits 0 regardless of its arguments. Real harness binaries are
/// unavailable in tests, and cmd.exe's .bat argument routing rejects the multiline prompt, so a
/// rustc-compiled helper is the only argument-agnostic exit-0 program we can produce.
fn exit_zero_binary(temp: &tempfile::TempDir) -> std::path::PathBuf {
    let source = temp.path().join("exit0.rs");
    fs::write(&source, "fn main() { std::process::exit(0) }\n").unwrap();
    let binary = temp
        .path()
        .join(if cfg!(windows) { "exit0.exe" } else { "exit0" });
    let status = Command::new("rustc")
        .arg("-O")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .status()
        .unwrap();
    assert!(status.success(), "rustc must produce the helper binary");
    binary
}

fn worker_harness_spec(repository: &Path, task: &Path) -> JobSpec {
    let worker_bin = env!("CARGO_BIN_EXE_liberado-harness-worker");
    transport::submit(transport::SubmitOptions {
        repository: repository.to_path_buf(),
        base_revision: "HEAD".to_string(),
        task_file: task.to_path_buf(),
        harnesses: vec![
            HarnessRequest {
                id: "liberado".to_string(),
                binary: Some(worker_bin.into()),
            },
            HarnessRequest {
                id: "pi".to_string(),
                binary: Some(worker_bin.into()),
            },
        ],
        run_order: vec!["liberado".to_string(), "pi".to_string()],
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
            compile_timeout_secs: 300,
            run_timeout_secs: 60,
            minimum_free_bytes: 0,
            verifier_repair_attempts: 0,
        },
        verifier: VerifierProfile::WorkspaceTests,
        task_aware_context: false,
        acceptance_overlay: None,
        experiment: None,
    })
    .expect("submit must queue")
}

#[test]
fn engine_runs_a_queued_job_to_a_terminal_failed_report_offline() {
    let temp = tempfile::tempdir().unwrap();
    let repository = scratch_repository(&temp);
    let task = temp.path().join("task.txt");
    fs::write(&task, "Compare both harnesses on this task offline.\n").unwrap();

    let spec = worker_harness_spec(&repository, &task);
    let policy = permissive_policy(&repository, "LIBERADO_HARNESS_E2E_KEY_A");
    unsafe { std::env::set_var("LIBERADO_HARNESS_E2E_KEY_A", "dummy-nonempty") };

    let store = JobStore::for_repository(&repository);
    let report = engine::execute(&store, &spec.job_id, &policy).expect("execute must finish");
    unsafe { std::env::remove_var("LIBERADO_HARNESS_E2E_KEY_A") };

    // Both harness adapters were invoked with an argument set the worker binary rejects, so the
    // run failed as a harness failure and the job landed terminal with a report on disk.
    assert_eq!(report.status, JobStatus::Failed);
    assert_eq!(report.failure_class, Some(FailureClass::HarnessFailure));
    assert!(!report.diagnostics.is_empty(), "{report:?}");
    let state = store.load_state(&spec.job_id).unwrap();
    assert!(state.status.is_terminal());
    assert!(store.job_root(&spec.job_id).join("report.json").is_file());
    assert!(store.load_report(&spec.job_id).is_ok());

    // The whole pipeline ran: immutable inputs verified, preflight recorded, experiment pinned,
    // harness artifacts normalized, build caches and worktrees cleaned up.
    let job_root = store.job_root(&spec.job_id);
    assert!(job_root.join("preflight.json").is_file());
    assert!(job_root.join("experiment.json").is_file());
    assert!(job_root.join("input/task.txt").is_file());
    for harness in ["liberado", "pi"] {
        assert!(
            job_root
                .join("artifacts/harnesses")
                .join(harness)
                .join("result.json")
                .is_file(),
            "{harness} result.json missing"
        );
    }
    assert!(!job_root.join("execution/targets").exists());
    assert!(!job_root.join("execution/worktrees/liberado").exists());
}

/// The detached worker is the production dispatch path (`submit` without `--no-spawn`): spawn the
/// binary, let it pick the job up from the queue, and wait for a terminal state.
#[test]
fn detached_worker_runs_a_queued_job_to_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let repository = scratch_repository(&temp);
    let task = temp.path().join("task.txt");
    fs::write(&task, "Compare both harnesses on this task offline.\n").unwrap();

    let spec = worker_harness_spec(&repository, &task);

    // The worker loads its own policy from the repository ACL file.
    let policy = permissive_policy(&repository, "LIBERADO_HARNESS_E2E_KEY_B");
    let policy_dir = repository.join(".liberado");
    fs::create_dir_all(&policy_dir).unwrap();
    fs::write(
        policy_dir.join("harness-worker.json"),
        serde_json::to_string_pretty(&policy).unwrap(),
    )
    .unwrap();

    unsafe { std::env::set_var("LIBERADO_HARNESS_E2E_KEY_B", "dummy-nonempty") };
    // Point at cargo's finished worker. Copying it next to this test binary races rustc
    // on `target/debug/deps/liberado-harness-worker` and fails Linux with ETXTBSY.
    unsafe {
        std::env::set_var(
            "LIBERADO_HARNESS_WORKER",
            env!("CARGO_BIN_EXE_liberado-harness-worker"),
        )
    };
    let store = JobStore::for_repository(&repository);
    let spawned = liberado_harness_eval::worker::spawn_executor(&repository, &spec.job_id);
    let waited = spawned.as_ref().ok().map(|_| {
        liberado_harness_eval::transport::await_terminal(&repository, &spec.job_id, None, None)
    });
    unsafe {
        std::env::remove_var("LIBERADO_HARNESS_E2E_KEY_B");
        std::env::remove_var("LIBERADO_HARNESS_WORKER");
    }
    spawned.expect("spawn must succeed");
    let terminal = waited
        .expect("spawn succeeded")
        .expect("job must reach a terminal state");

    // The detached worker ran the same offline pipeline and classified the harness failures.
    assert_eq!(terminal.status, JobStatus::Failed);
    // Report.json is written before the terminal state, but cross-process filesystem visibility
    // can race: await_terminal sees the state file, then the test reads report.json before the
    // worker's rename is visible. Retry a few times.
    let report = (0..10)
        .map(|_| {
            store.load_report(&spec.job_id).or_else(|_| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                store.load_report(&spec.job_id)
            })
        })
        .find(Result::is_ok)
        .expect("report.json never appeared after terminal state")
        .unwrap();
    assert_eq!(report.failure_class, Some(FailureClass::HarnessFailure));
    assert!(store.job_root(&spec.job_id).join("report.json").is_file());
}

/// Both harnesses exit 0 (rustc-compiled helper) and the common verifier passes on the tiny crate,
/// so the coordinator lands on Succeeded — the success classification, report, and run_job exit.
#[test]
fn engine_succeeds_when_both_harnesses_pass() {
    let temp = tempfile::tempdir().unwrap();
    let repository = scratch_repository(&temp);
    let task = temp.path().join("task.txt");
    fs::write(&task, "Compare both harnesses on this task offline.\n").unwrap();

    let ok_harness = exit_zero_binary(&temp);
    let spec = transport::submit(transport::SubmitOptions {
        repository: repository.clone(),
        base_revision: "HEAD".to_string(),
        task_file: task,
        harnesses: vec![
            HarnessRequest {
                id: "liberado".to_string(),
                binary: Some(ok_harness.clone()),
            },
            HarnessRequest {
                id: "pi".to_string(),
                binary: Some(ok_harness),
            },
        ],
        run_order: vec!["liberado".to_string(), "pi".to_string()],
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
            compile_timeout_secs: 300,
            run_timeout_secs: 60,
            minimum_free_bytes: 0,
            verifier_repair_attempts: 0,
        },
        verifier: VerifierProfile::WorkspaceTests,
        task_aware_context: false,
        acceptance_overlay: None,
        experiment: None,
    })
    .expect("submit must queue");

    let policy = permissive_policy(&repository, "LIBERADO_HARNESS_E2E_KEY_C");
    // run_job loads its policy from the repository ACL file, as production does.
    let policy_dir = repository.join(".liberado");
    fs::create_dir_all(&policy_dir).unwrap();
    fs::write(
        policy_dir.join("harness-worker.json"),
        serde_json::to_string_pretty(&policy).unwrap(),
    )
    .unwrap();

    unsafe { std::env::set_var("LIBERADO_HARNESS_E2E_KEY_C", "dummy-nonempty") };
    let store = JobStore::for_repository(&repository);
    let outcome = liberado_harness_eval::worker::run_job(&repository, &spec.job_id);
    unsafe { std::env::remove_var("LIBERADO_HARNESS_E2E_KEY_C") };
    assert!(
        outcome.is_ok(),
        "run_job must accept a succeeded report: {outcome:?}"
    );

    let report = store.load_report(&spec.job_id).unwrap();
    assert_eq!(report.status, JobStatus::Succeeded);
    assert_eq!(report.failure_class, None);
    let state = store.load_state(&spec.job_id).unwrap();
    assert_eq!(state.status, JobStatus::Succeeded);
    for harness in report.harnesses.values() {
        assert_eq!(harness.exit_code, Some(0));
        assert_eq!(harness.verifier_exit_code, Some(0));
        assert!(harness.accepted);
    }
}

/// The immutable captured inputs are validated before any paid work: tampering with the captured
/// task after submit must fail the job as a host infrastructure failure.
#[test]
fn engine_rejects_tampered_captured_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let repository = scratch_repository(&temp);
    let task = temp.path().join("task.txt");
    fs::write(&task, "Compare both harnesses on this task offline.\n").unwrap();

    let spec = worker_harness_spec(&repository, &task);
    let policy = permissive_policy(&repository, "LIBERADO_HARNESS_E2E_KEY_D");

    let store = JobStore::for_repository(&repository);
    // Tamper with the captured input after it was accepted.
    fs::write(
        store.job_root(&spec.job_id).join("input/task.txt"),
        "tampered content\n",
    )
    .unwrap();

    unsafe { std::env::set_var("LIBERADO_HARNESS_E2E_KEY_D", "dummy-nonempty") };
    let report = engine::execute(&store, &spec.job_id, &policy).expect("execute must finish");
    unsafe { std::env::remove_var("LIBERADO_HARNESS_E2E_KEY_D") };

    assert_eq!(report.status, JobStatus::Failed);
    assert_eq!(
        report.failure_class,
        Some(FailureClass::HostInfrastructureFailure)
    );
    assert!(
        report.diagnostics[0].contains("captured input validation failed"),
        "{:?}",
        report.diagnostics
    );
}
