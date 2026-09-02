//! Split from `job_cli.rs` for module-health boundaries.

use super::*;
use crate::journal::{JobStore, RunnerLock};
use std::fs;
use std::path::Path;
use std::process::Command;

fn git(repository: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .status()
        .unwrap()
        .success()
}

/// A git repository with one committed file. Identity is passed per-command (the repo
/// convention: no runner has user.email/user.name, and env mutation races under parallel tests).
fn committed_repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(&repository).unwrap();
    assert!(git(&repository, &["init"]));
    fs::write(repository.join("README.md"), "base\n").unwrap();
    assert!(git(&repository, &["add", "README.md"]));
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
    temp
}

fn task_file(temp: &tempfile::TempDir) -> std::path::PathBuf {
    let task = temp.path().join("task.txt");
    fs::write(&task, "Compare the two harnesses on this task.\n").unwrap();
    task
}

#[test]
fn usage_lists_every_subcommand_and_its_flags() {
    let text = usage();
    for subcommand in ["submit", "doctor", "status", "await", "cancel", "report"] {
        assert!(text.contains(subcommand), "missing {subcommand}");
    }
    for flag in [
        "--task",
        "--source",
        "--wait",
        "--no-spawn",
        "--timeout-secs",
    ] {
        assert!(text.contains(flag), "missing {flag}");
    }
}

#[test]
fn run_rejects_unknown_subcommands_with_usage() {
    for args in [vec![], vec!["bogus".to_string()], vec!["-h".to_string()]] {
        let err = run(&args).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("usage:"), "{text}");
        assert!(text.contains("compare submit"), "{text}");
    }
}

#[test]
fn run_dispatches_each_subcommand() {
    // Every subcommand fails at its own argument gate with a distinct message — proving the
    // dispatcher routed there rather than falling through to usage.
    let err = run(&["submit".to_string()]).unwrap_err();
    assert!(err.to_string().contains("--task"), "{err}");
    let err = run(&["doctor".to_string()]).unwrap_err();
    assert!(err.to_string().contains("--task"), "{err}");
    for subcommand in ["status", "await", "cancel", "report"] {
        let err = run(&[subcommand.to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("job id is required"),
            "{subcommand}: {err}"
        );
    }
}

#[test]
fn positive_integers_reject_zero_negative_and_garbage() {
    assert_eq!(positive_u64("5", "--n").unwrap(), 5);
    assert_eq!(
        positive_u64("18446744073709551615", "--n").unwrap(),
        u64::MAX
    );
    for bad in ["0", "-1", "abc", "1.5", "18446744073709551616"] {
        assert!(positive_u64(bad, "--n").is_err(), "{bad}");
    }
    assert_eq!(positive_u32("5", "--n").unwrap(), 5);
    for bad in ["0", "-1", "abc", "4294967296"] {
        assert!(positive_u32(bad, "--n").is_err(), "{bad}");
    }
    let err = positive_u64("0", "--max-turns").unwrap_err().to_string();
    assert!(err.contains("--max-turns"), "{err}");
    assert!(err.contains("positive integer"), "{err}");
}

#[test]
fn next_consumes_the_following_argument() {
    let args = vec!["--a".to_string(), "value".to_string()];
    let mut index = 0;
    assert_eq!(next(&args, &mut index, "--a").unwrap(), "value");
    assert_eq!(index, 1);
    assert!(next(&args, &mut index, "--b").is_err());
    let err = next(&args, &mut index, "--b").unwrap_err().to_string();
    assert!(err.contains("--b requires a value"), "{err}");
}

#[test]
fn common_job_args_parses_id_source_and_timeouts() {
    let id = JobId::new().to_string();
    let real = tempfile::tempdir().unwrap();
    let parsed = common_job_args(
        &[
            "--source".into(),
            real.path().to_string_lossy().into_owned(),
            id.clone(),
        ],
        false,
    )
    .unwrap();
    assert_eq!(parsed.repository, real.path().canonicalize().unwrap());
    assert_eq!(parsed.job_id.0, id);
    assert!(parsed.timeout.is_none());

    let parsed = common_job_args(
        &[
            id.clone(),
            "--timeout-secs".into(),
            "30".into(),
            "--stall-secs".into(),
            "5".into(),
        ],
        true,
    )
    .unwrap();
    assert_eq!(parsed.timeout, Some(30));
    assert_eq!(parsed.stall_secs, Some(5));

    // Without allow_timeout the flags are rejected, not silently accepted.
    let err =
        common_job_args(&[id.clone(), "--timeout-secs".into(), "30".into()], false).unwrap_err();
    assert!(err.to_string().contains("unknown argument"), "{err}");

    for bad in [
        vec!["--source".to_string()],
        vec![id.clone(), id.clone()],
        vec!["--bogus".to_string(), id.clone()],
        vec![],
    ] {
        assert!(common_job_args(&bad, false).is_err(), "{bad:?}");
    }
    let err = common_job_args(&[], false).unwrap_err().to_string();
    assert!(err.contains("job id is required"), "{err}");
}

#[test]
fn submit_rejects_unknown_flags_and_bad_numbers() {
    let err = run(&["submit".into(), "--bogus".into()]).unwrap_err();
    assert!(
        err.to_string().contains("unknown compare submit argument"),
        "{err}"
    );
    for flag in [
        "--max-turns",
        "--compile-timeout-secs",
        "--minimum-free-gib",
    ] {
        let err = run(&["submit".into(), flag.into(), "0".into()]).unwrap_err();
        assert!(
            err.to_string().contains("positive integer"),
            "{flag}: {err}"
        );
    }
    let err = run(&["submit".into(), "--max-turns".into(), "abc".into()]).unwrap_err();
    assert!(err.to_string().contains("positive integer"), "{err}");
    // --verifier-repair-attempts takes any non-negative integer.
    let err = run(&[
        "submit".into(),
        "--verifier-repair-attempts".into(),
        "-1".into(),
    ])
    .unwrap_err();
    assert!(err.to_string().contains("non-negative integer"), "{err}");
    // --task requires a value at the end of the command line.
    let err = run(&["submit".into(), "--task".into()]).unwrap_err();
    assert!(err.to_string().contains("requires a value"), "{err}");
}

#[test]
fn submit_help_prints_usage_and_succeeds() {
    for flag in ["-h", "--help"] {
        run(&["submit".into(), flag.into()]).expect("help must exit 0");
    }
}

#[test]
fn submit_requires_hypothesis_and_variable_together() {
    let temp = committed_repo();
    let repository = temp.path().join("repo");
    let task = task_file(&temp);
    let err = run(&[
        "submit".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        "--task".into(),
        task.to_string_lossy().into_owned(),
        "--hypothesis".into(),
        "h".into(),
    ])
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("--hypothesis and --variable must be supplied together"),
        "{err}"
    );
    // Supplied together, the pair is accepted (job is queued, no spawn).
    run(&[
        "submit".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        "--task".into(),
        task.to_string_lossy().into_owned(),
        "--commit".into(),
        "HEAD".into(),
        "--hypothesis".into(),
        "h".into(),
        "--variable".into(),
        "v".into(),
        "--no-spawn".into(),
    ])
    .expect("paired experiment must queue");
    let store = JobStore::for_repository(&repository);
    assert_eq!(store.accepted_jobs().unwrap().len(), 1);
}

#[test]
fn submit_refuses_while_the_runner_lock_is_held() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(&repository).unwrap();
    let store = JobStore::for_repository(&repository);
    fs::create_dir_all(store.root()).unwrap();
    let _lock = RunnerLock::acquire(&store).unwrap();
    let task = task_file(&temp);
    let err = run(&[
        "submit".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        "--task".into(),
        task.to_string_lossy().into_owned(),
    ])
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("another comparison is already running"),
        "{err}"
    );
}

#[test]
fn submit_queues_a_job_with_immutable_inputs() {
    let temp = committed_repo();
    let repository = temp.path().join("repo");
    let task = task_file(&temp);
    run(&[
        "submit".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        "--task".into(),
        task.to_string_lossy().into_owned(),
        "--commit".into(),
        "HEAD".into(),
        "--max-turns".into(),
        "50".into(),
        "--no-spawn".into(),
    ])
    .expect("submit must queue");
    let store = JobStore::for_repository(&repository);
    let jobs = store.accepted_jobs().unwrap();
    assert_eq!(jobs.len(), 1);
    let spec = store.load_spec(&jobs[0]).unwrap();
    assert_eq!(spec.task.text, "Compare the two harnesses on this task.\n");
    assert_eq!(
        spec.harnesses
            .iter()
            .map(|h| h.id.as_str())
            .collect::<Vec<_>>(),
        ["liberado", "pi"]
    );
    assert_eq!(spec.model.max_turns, 50);
    // The run order is a permutation of the harness ids, recorded but not part of the id.
    assert_eq!(spec.run_order.len(), 2);
    assert_ne!(spec.run_order[0], spec.run_order[1]);
    // The captured task sits on disk next to job.json.
    let captured = fs::read_to_string(store.job_root(&jobs[0]).join("input/task.txt")).unwrap();
    assert_eq!(captured, spec.task.text);
}

#[test]
fn status_await_and_cancel_round_trip_a_queued_job() {
    let temp = committed_repo();
    let repository = temp.path().join("repo");
    let task = task_file(&temp);
    run(&[
        "submit".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        "--task".into(),
        task.to_string_lossy().into_owned(),
        "--commit".into(),
        "HEAD".into(),
        "--no-spawn".into(),
    ])
    .expect("submit must queue");
    let store = JobStore::for_repository(&repository);
    let jobs = store.accepted_jobs().unwrap();
    let id = jobs[0].to_string();

    // status reports the accepted state.
    let state = transport::status(&repository, &jobs[0]).unwrap();
    assert_eq!(state.status, JobStatus::Accepted);

    // await with a short timeout fails deterministically: the job never runs (no spawn).
    let err = run(&[
        "await".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        id.clone(),
        "--timeout-secs".into(),
        "1".into(),
    ])
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("did not finish before the await timeout"),
        "{err}"
    );

    // cancel records a cancellation request rather than mutating status in place.
    run(&[
        "cancel".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        id.clone(),
    ])
    .expect("cancel must succeed");
    assert!(store.cancellation_requested(&jobs[0]));

    // report on a job with no report yet fails loudly rather than printing nothing.
    let err = run(&[
        "report".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        id,
    ])
    .unwrap_err();
    assert!(!err.to_string().is_empty());
}

#[test]
fn submit_wait_times_out_cleanly_when_the_job_never_runs() {
    // `--wait` with `--no-spawn` on a queued job must surface the await timeout as an
    // error, not hang forever — the same terminal contract `await` exercises directly.
    let temp = committed_repo();
    let repository = temp.path().join("repo");
    let task = task_file(&temp);
    let err = run(&[
        "submit".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        "--task".into(),
        task.to_string_lossy().into_owned(),
        "--commit".into(),
        "HEAD".into(),
        "--no-spawn".into(),
        "--wait".into(),
        "--timeout-secs".into(),
        "1".into(),
    ])
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("did not finish before the await timeout"),
        "{err}"
    );
}

#[test]
fn doctor_rejects_bad_flags_and_reports_missing_policy() {
    let err = run(&["doctor".into(), "--bogus".into()]).unwrap_err();
    assert!(
        err.to_string().contains("unknown compare doctor argument"),
        "{err}"
    );
    let err = run(&["doctor".into(), "--max-turns".into(), "0".into()]).unwrap_err();
    assert!(err.to_string().contains("positive integer"), "{err}");
    run(&["doctor".into(), "--help".into()]).expect("help must exit 0");

    // With a real repository and task, doctor fails at the missing worker policy — before any
    // harness binary check, so this is CI-stable (no pi on PATH).
    let temp = committed_repo();
    let repository = temp.path().join("repo");
    let task = task_file(&temp);
    let err = run(&[
        "doctor".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        "--task".into(),
        task.to_string_lossy().into_owned(),
        "--commit".into(),
        "HEAD".into(),
    ])
    .unwrap_err();
    assert!(
        err.to_string().contains("worker policy is unavailable"),
        "{err}"
    );
}

fn write_worker_policy(repository: &Path, allow_binary_overrides: bool) {
    let mut policy = WorkerPolicy::for_repository(repository.to_path_buf());
    policy.allow_binary_overrides = allow_binary_overrides;
    policy.minimum_free_bytes = 0;
    policy.estimated_build_bytes_per_harness = 0;
    let dir = repository.join(".liberado");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("harness-worker.json"),
        serde_json::to_string_pretty(&policy).unwrap(),
    )
    .unwrap();
}

#[test]
fn submit_default_stays_two_way_liberado_pi() {
    let temp = committed_repo();
    let repository = temp.path().join("repo");
    let task = task_file(&temp);
    run(&[
        "submit".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        "--task".into(),
        task.to_string_lossy().into_owned(),
        "--commit".into(),
        "HEAD".into(),
        "--no-spawn".into(),
    ])
    .expect("two-way submit must queue");
    let store = JobStore::for_repository(&repository);
    let spec = store.load_spec(&store.accepted_jobs().unwrap()[0]).unwrap();
    let ids: Vec<&str> = spec.harnesses.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, ["liberado", "pi"]);
}

#[test]
fn submit_four_way_when_c3_bins_are_named() {
    let temp = committed_repo();
    let repository = temp.path().join("repo");
    let task = task_file(&temp);
    let hermes = temp.path().join("hermes");
    let dcode = temp.path().join("dcode");
    fs::write(&hermes, "x").unwrap();
    fs::write(&dcode, "x").unwrap();
    run(&[
        "submit".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        "--task".into(),
        task.to_string_lossy().into_owned(),
        "--commit".into(),
        "HEAD".into(),
        "--hermes-bin".into(),
        hermes.to_string_lossy().into_owned(),
        "--deep-agents-bin".into(),
        dcode.to_string_lossy().into_owned(),
        "--hermes-git-sha".into(),
        "aaa111".into(),
        "--deep-agents-git-sha".into(),
        "bbb222".into(),
        "--no-spawn".into(),
    ])
    .expect("four-way submit must queue");
    let store = JobStore::for_repository(&repository);
    let spec = store.load_spec(&store.accepted_jobs().unwrap()[0]).unwrap();
    let mut ids: Vec<&str> = spec.harnesses.iter().map(|h| h.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["deepagents", "hermes", "liberado", "pi"]);
    let hermes = spec.harnesses.iter().find(|h| h.id == "hermes").unwrap();
    assert_eq!(hermes.git_sha.as_deref(), Some("aaa111"));
    assert_eq!(spec.run_order.len(), 4);
}

#[test]
fn doctor_four_way_fails_clearly_when_a_bin_is_missing() {
    let temp = committed_repo();
    let repository = temp.path().join("repo");
    write_worker_policy(&repository, true);
    let task = task_file(&temp);
    let ok = temp.path().join("ok-bin");
    fs::write(&ok, "x").unwrap();
    // A directory canonicalizes but is not a file, so preflight fails before any credential
    // lookup or model call.
    let missing = temp.path().join("missing-hermes");
    fs::create_dir(&missing).unwrap();
    let err = run(&[
        "doctor".into(),
        "--source".into(),
        repository.to_string_lossy().into_owned(),
        "--task".into(),
        task.to_string_lossy().into_owned(),
        "--commit".into(),
        "HEAD".into(),
        "--harnesses".into(),
        "liberado,pi,hermes,deepagents".into(),
        "--pi-bin".into(),
        ok.to_string_lossy().into_owned(),
        "--hermes-bin".into(),
        missing.to_string_lossy().into_owned(),
        "--deep-agents-bin".into(),
        ok.to_string_lossy().into_owned(),
    ])
    .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("does not exist"), "{text}");
    assert!(text.to_ascii_lowercase().contains("hermes"), "{text}");
}
