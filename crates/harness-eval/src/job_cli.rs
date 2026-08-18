//! Argument adapter for durable comparison jobs.

use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use crate::contract::*;
use crate::journal::{JobStore, RunnerLock};
use crate::{preflight, repository_root, transport, worker};

pub fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    match args.first().map(String::as_str) {
        Some("submit") => submit(&args[1..]),
        Some("doctor") => doctor(&args[1..]),
        Some("status") => status(&args[1..]),
        Some("await") => await_job(&args[1..]),
        Some("cancel") => cancel(&args[1..]),
        Some("report") => report(&args[1..]),
        _ => Err(usage().into()),
    }
}

pub fn usage() -> &'static str {
    "usage:\n  \
     liberado coder compare submit --task <file> [pins] [--wait] [--no-spawn]\n  \
     liberado coder compare doctor --task <file> [pins]\n  \
     liberado coder compare status <job-id> [--source <repo>]\n  \
     liberado coder compare await <job-id> [--timeout-secs <n>] [--stall-secs <n>] [--source <repo>]\n  \
     liberado coder compare cancel <job-id> [--source <repo>]\n  \
     liberado coder compare report <job-id> [--json] [--source <repo>]"
}

fn submit(args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut repository = None;
    let mut task = None;
    let mut commit = "main".to_string();
    let mut provider = "openrouter".to_string();
    let mut model = "deepseek/deepseek-v4-flash".to_string();
    let mut base_url = "https://openrouter.ai/api/v1".to_string();
    let mut credential_alias = "openrouter-default".to_string();
    let mut thinking = "high".to_string();
    let mut limits = ResourceLimits::default();
    let mut max_turns = 400;
    let mut task_aware_context = false;
    let mut acceptance_overlay = None;
    let mut liberado_bin = None;
    let mut pi_bin = None;
    let mut hypothesis = None;
    let mut variable = None;
    let mut wait = false;
    let mut wait_timeout = None;
    let mut no_spawn = false;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--source" => repository = Some(PathBuf::from(next(args, &mut index, flag)?)),
            "--task" => task = Some(PathBuf::from(next(args, &mut index, flag)?)),
            "--commit" => commit = next(args, &mut index, flag)?.to_string(),
            "--provider" => provider = next(args, &mut index, flag)?.to_string(),
            "--model" => model = next(args, &mut index, flag)?.to_string(),
            "--base-url" => base_url = next(args, &mut index, flag)?.to_string(),
            "--credential" => credential_alias = next(args, &mut index, flag)?.to_string(),
            "--thinking" => thinking = next(args, &mut index, flag)?.to_string(),
            "--max-turns" => max_turns = positive_u32(next(args, &mut index, flag)?, flag)?,
            "--compile-timeout-secs" => {
                limits.compile_timeout_secs = positive_u64(next(args, &mut index, flag)?, flag)?
            }
            "--run-timeout-secs" => {
                limits.run_timeout_secs = positive_u64(next(args, &mut index, flag)?, flag)?
            }
            "--verifier-repair-attempts" => {
                limits.verifier_repair_attempts = next(args, &mut index, flag)?
                    .parse()
                    .map_err(|_| format!("{flag} must be a non-negative integer"))?
            }
            "--minimum-free-gib" => {
                limits.minimum_free_bytes = positive_u64(next(args, &mut index, flag)?, flag)?
                    .saturating_mul(1024 * 1024 * 1024)
            }
            "--task-aware-context" => task_aware_context = true,
            "--acceptance-overlay" => {
                acceptance_overlay = Some(PathBuf::from(next(args, &mut index, flag)?))
            }
            "--liberado-bin" => liberado_bin = Some(PathBuf::from(next(args, &mut index, flag)?)),
            "--pi-bin" => pi_bin = Some(PathBuf::from(next(args, &mut index, flag)?)),
            "--hypothesis" => hypothesis = Some(next(args, &mut index, flag)?.to_string()),
            "--variable" => variable = Some(next(args, &mut index, flag)?.to_string()),
            "--wait" => wait = true,
            "--no-spawn" => no_spawn = true,
            "--timeout-secs" => {
                wait_timeout = Some(positive_u64(next(args, &mut index, flag)?, flag)?)
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other => return Err(format!("unknown compare submit argument: {other}").into()),
        }
        index += 1;
    }
    let repository = repository.unwrap_or(repository_root()?).canonicalize()?;
    let task_file = task.ok_or("compare submit requires --task <file>")?;
    // Refuse a new run while another comparison holds the runner lock. One at a time is a
    // measurement policy, not a limitation.
    let store = JobStore::for_repository(&repository);
    if RunnerLock::is_held(&store) {
        return Err("another comparison is already running in this repository".into());
    }
    let experiment = match (hypothesis, variable) {
        (None, None) => None,
        (Some(hypothesis), Some(variable)) => Some(Experiment {
            hypothesis,
            variable,
        }),
        _ => return Err("--hypothesis and --variable must be supplied together".into()),
    };
    // Alternate the run order per job so the systematic "first harness" bias cancels out across
    // jobs. The order is recorded in report.json; it is not part of the experiment id.
    let run_order = alternate_run_order(JobStore::for_repository(&repository).job_count()?);
    let spec = transport::submit(transport::SubmitOptions {
        repository: repository.clone(),
        base_revision: commit,
        task_file,
        harnesses: vec![
            HarnessRequest {
                id: "liberado".to_string(),
                binary: liberado_bin,
            },
            HarnessRequest {
                id: "pi".to_string(),
                binary: pi_bin,
            },
        ],
        run_order,
        model: ModelPins {
            provider,
            model,
            base_url,
            credential_alias,
            thinking,
            max_turns,
            sampling: SAMPLING_OMITTED.to_string(),
        },
        limits,
        verifier: VerifierProfile::WorkspaceTests,
        task_aware_context,
        acceptance_overlay,
        experiment,
    })?;
    println!("{}", spec.job_id);
    println!("experiment_id={}", spec.experiment_id);
    println!("status=accepted");
    if !no_spawn {
        worker::spawn_executor(&repository, &spec.job_id)?;
    }
    if wait {
        let state = transport::await_terminal(
            &repository,
            &spec.job_id,
            wait_timeout.map(Duration::from_secs),
            None,
        )?;
        let report_path = JobStore::for_repository(&repository)
            .job_root(&spec.job_id)
            .join("report.md");
        println!("status={:?}", state.status);
        println!("report={}", report_path.display());
        if state.status != JobStatus::Succeeded {
            return Err(format!("job {} finished as {:?}", spec.job_id, state.status).into());
        }
    }
    Ok(())
}

fn doctor(args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut repository = None;
    let mut task = None;
    let mut commit = "main".to_string();
    let mut provider = "openrouter".to_string();
    let mut model = "deepseek/deepseek-v4-flash".to_string();
    let mut base_url = "https://openrouter.ai/api/v1".to_string();
    let mut credential_alias = "openrouter-default".to_string();
    let mut thinking = "high".to_string();
    let mut limits = ResourceLimits::default();
    let mut max_turns = 400;
    let mut task_aware_context = false;
    let mut acceptance_overlay = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--source" => repository = Some(PathBuf::from(next(args, &mut index, flag)?)),
            "--task" => task = Some(PathBuf::from(next(args, &mut index, flag)?)),
            "--commit" => commit = next(args, &mut index, flag)?.to_string(),
            "--provider" => provider = next(args, &mut index, flag)?.to_string(),
            "--model" => model = next(args, &mut index, flag)?.to_string(),
            "--base-url" => base_url = next(args, &mut index, flag)?.to_string(),
            "--credential" => credential_alias = next(args, &mut index, flag)?.to_string(),
            "--thinking" => thinking = next(args, &mut index, flag)?.to_string(),
            "--max-turns" => max_turns = positive_u32(next(args, &mut index, flag)?, flag)?,
            "--compile-timeout-secs" => {
                limits.compile_timeout_secs = positive_u64(next(args, &mut index, flag)?, flag)?
            }
            "--run-timeout-secs" => {
                limits.run_timeout_secs = positive_u64(next(args, &mut index, flag)?, flag)?
            }
            "--minimum-free-gib" => {
                limits.minimum_free_bytes = positive_u64(next(args, &mut index, flag)?, flag)?
                    .saturating_mul(1024 * 1024 * 1024)
            }
            "--task-aware-context" => task_aware_context = true,
            "--acceptance-overlay" => {
                acceptance_overlay = Some(PathBuf::from(next(args, &mut index, flag)?))
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other => return Err(format!("unknown compare doctor argument: {other}").into()),
        }
        index += 1;
    }
    let repository = repository.unwrap_or(repository_root()?).canonicalize()?;
    let task_file = task.ok_or("compare doctor requires --task <file>")?;
    let options = transport::SubmitOptions {
        repository: repository.clone(),
        base_revision: commit,
        task_file,
        harnesses: vec![
            HarnessRequest {
                id: "liberado".into(),
                binary: None,
            },
            HarnessRequest {
                id: "pi".into(),
                binary: None,
            },
        ],
        run_order: default_run_order(),
        model: ModelPins {
            provider,
            model,
            base_url,
            credential_alias,
            thinking,
            max_turns,
            sampling: SAMPLING_OMITTED.to_string(),
        },
        limits,
        verifier: VerifierProfile::WorkspaceTests,
        task_aware_context,
        acceptance_overlay,
        experiment: None,
    };
    let spec = transport::build_spec(options)?;
    let policy_path = transport::policy_path(&repository);
    let policy = transport::load_policy(&policy_path).map_err(|error| {
        format!(
            "worker policy is unavailable at {}: {error}",
            policy_path.display()
        )
    })?;
    let report = preflight::run(&spec, &policy)?;
    println!("doctor=ok");
    println!("repository={}", report.0.repository.display());
    println!("base_commit={}", report.0.base_commit);
    println!("free_bytes={}", report.0.free_bytes);
    println!(
        "estimated_required_bytes={}",
        report.0.estimated_required_bytes
    );
    println!("credential_environment={}", report.0.credential_environment);
    Ok(())
}

fn status(args: &[String]) -> Result<(), Box<dyn Error>> {
    let job = common_job_args(args, false)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&transport::status(&job.repository, &job.job_id)?)?
    );
    Ok(())
}

fn await_job(args: &[String]) -> Result<(), Box<dyn Error>> {
    let job = common_job_args(args, true)?;
    let state = transport::await_terminal(
        &job.repository,
        &job.job_id,
        job.timeout.map(Duration::from_secs),
        job.stall_secs,
    )?;
    if state.status == JobStatus::Succeeded {
        Ok(())
    } else {
        Err(format!("job {} finished as {:?}", job.job_id, state.status).into())
    }
}

fn cancel(args: &[String]) -> Result<(), Box<dyn Error>> {
    let job = common_job_args(args, false)?;
    transport::cancel(&job.repository, &job.job_id)?;
    println!("cancel requested: {}", job.job_id);
    Ok(())
}

fn report(args: &[String]) -> Result<(), Box<dyn Error>> {
    let json = args.iter().any(|argument| argument == "--json");
    let filtered: Vec<_> = args
        .iter()
        .filter(|argument| argument.as_str() != "--json")
        .cloned()
        .collect();
    let job = common_job_args(&filtered, false)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&transport::report(&job.repository, &job.job_id)?)?
        );
    } else {
        let path = crate::journal::JobStore::for_repository(&job.repository)
            .job_root(&job.job_id)
            .join("report.md");
        print!("{}", std::fs::read_to_string(path)?);
    }
    Ok(())
}

#[derive(Debug)]
struct JobArgs {
    repository: PathBuf,
    job_id: JobId,
    timeout: Option<u64>,
    stall_secs: Option<u64>,
}

fn common_job_args(args: &[String], allow_timeout: bool) -> Result<JobArgs, Box<dyn Error>> {
    let mut repository = None;
    let mut job_id = None;
    let mut timeout = None;
    let mut stall_secs = None;
    let mut index = 0;
    while index < args.len() {
        let value = args[index].as_str();
        match value {
            "--source" => repository = Some(PathBuf::from(next(args, &mut index, value)?)),
            "--timeout-secs" if allow_timeout => {
                timeout = Some(positive_u64(next(args, &mut index, value)?, value)?)
            }
            "--stall-secs" if allow_timeout => {
                stall_secs = Some(positive_u64(next(args, &mut index, value)?, value)?)
            }
            flag if flag.starts_with('-') => return Err(format!("unknown argument: {flag}").into()),
            value => {
                if job_id.is_some() {
                    return Err("command accepts one job id".into());
                }
                job_id = Some(JobId::parse(value)?);
            }
        }
        index += 1;
    }
    Ok(JobArgs {
        repository: repository.unwrap_or(repository_root()?).canonicalize()?,
        job_id: job_id.ok_or("job id is required")?,
        timeout,
        stall_secs,
    })
}

fn next<'a>(args: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str, Box<dyn Error>> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires a value").into())
}

fn positive_u64(value: &str, flag: &str) -> Result<u64, Box<dyn Error>> {
    let value: u64 = value
        .parse()
        .map_err(|_| format!("{flag} must be a positive integer"))?;
    if value == 0 {
        return Err(format!("{flag} must be a positive integer").into());
    }
    Ok(value)
}

fn positive_u32(value: &str, flag: &str) -> Result<u32, Box<dyn Error>> {
    let value: u32 = value
        .parse()
        .map_err(|_| format!("{flag} must be a positive integer"))?;
    if value == 0 {
        return Err(format!("{flag} must be a positive integer").into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
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
        let err = common_job_args(&[id.clone(), "--timeout-secs".into(), "30".into()], false)
            .unwrap_err();
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
}
