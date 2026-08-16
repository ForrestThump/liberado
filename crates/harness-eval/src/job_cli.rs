//! Argument adapter for durable comparison jobs.

use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use crate::contract::*;
use crate::journal::JobStore;
use crate::{preflight, repository_root, transport, worker};

pub fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    match args.first().map(String::as_str) {
        Some("submit") => submit(&args[1..]),
        Some("doctor") => doctor(&args[1..]),
        Some("status") => status(&args[1..]),
        Some("await") => await_job(&args[1..]),
        Some("cancel") => cancel(&args[1..]),
        Some("report") => report(&args[1..]),
        Some("worker") => worker_command(&args[1..]),
        _ => Err(usage().into()),
    }
}

pub fn usage() -> &'static str {
    "usage:\n  \
     liberado coder compare submit --task <file> [pins] [--wait]\n  \
     liberado coder compare doctor --task <file> [pins]\n  \
     liberado coder compare status <job-id> [--source <repo>]\n  \
     liberado coder compare await <job-id> [--timeout-secs <n>] [--source <repo>]\n  \
     liberado coder compare cancel <job-id> [--source <repo>]\n  \
     liberado coder compare report <job-id> [--json] [--source <repo>]\n  \
     liberado coder compare worker install|start|once [--source <repo>] [--worker-bin <path>]"
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
    let repository = repository.unwrap_or(repository_root()?);
    let task_file = task.ok_or("compare submit requires --task <file>")?;
    let experiment = match (hypothesis, variable) {
        (None, None) => None,
        (Some(hypothesis), Some(variable)) => Some(Experiment {
            hypothesis,
            variable,
        }),
        _ => return Err("--hypothesis and --variable must be supplied together".into()),
    };
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
    if wait {
        let state = transport::await_terminal(
            &repository,
            &spec.job_id,
            wait_timeout.map(Duration::from_secs),
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
    let (repository, job_id, _) = common_job_args(args, false)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&transport::status(&repository, &job_id)?)?
    );
    Ok(())
}

fn await_job(args: &[String]) -> Result<(), Box<dyn Error>> {
    let (repository, job_id, timeout) = common_job_args(args, true)?;
    let state = transport::await_terminal(&repository, &job_id, timeout.map(Duration::from_secs))?;
    if state.status == JobStatus::Succeeded {
        Ok(())
    } else {
        Err(format!("job {} finished as {:?}", job_id, state.status).into())
    }
}

fn cancel(args: &[String]) -> Result<(), Box<dyn Error>> {
    let (repository, job_id, _) = common_job_args(args, false)?;
    transport::cancel(&repository, &job_id)?;
    println!("cancel requested: {job_id}");
    Ok(())
}

fn report(args: &[String]) -> Result<(), Box<dyn Error>> {
    let json = args.iter().any(|argument| argument == "--json");
    let filtered: Vec<_> = args
        .iter()
        .filter(|argument| argument.as_str() != "--json")
        .cloned()
        .collect();
    let (repository, job_id, _) = common_job_args(&filtered, false)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&transport::report(&repository, &job_id)?)?
        );
    } else {
        let path = crate::journal::JobStore::for_repository(&repository)
            .job_root(&job_id)
            .join("report.md");
        print!("{}", std::fs::read_to_string(path)?);
    }
    Ok(())
}

fn worker_command(args: &[String]) -> Result<(), Box<dyn Error>> {
    let action = args.first().ok_or(usage())?;
    let mut repository = None;
    let mut worker_binary = None;
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--source" => repository = Some(PathBuf::from(next(args, &mut index, flag)?)),
            "--worker-bin" => worker_binary = Some(PathBuf::from(next(args, &mut index, flag)?)),
            other => return Err(format!("unknown compare worker argument: {other}").into()),
        }
        index += 1;
    }
    let repository = repository.unwrap_or(repository_root()?).canonicalize()?;
    let policy = transport::policy_path(&repository);
    match action.as_str() {
        "install" => {
            let path = worker::install(&repository, worker_binary.as_deref())?;
            println!("worker installed and started");
            println!("policy={}", path.display());
        }
        "start" => {
            worker::start(&policy, worker_binary.as_deref())?;
            println!("worker started");
        }
        "once" => worker::run(&policy, true)?,
        _ => return Err(usage().into()),
    }
    Ok(())
}

fn common_job_args(
    args: &[String],
    allow_timeout: bool,
) -> Result<(PathBuf, JobId, Option<u64>), Box<dyn Error>> {
    let mut repository = None;
    let mut job_id = None;
    let mut timeout = None;
    let mut index = 0;
    while index < args.len() {
        let value = args[index].as_str();
        match value {
            "--source" => repository = Some(PathBuf::from(next(args, &mut index, value)?)),
            "--timeout-secs" if allow_timeout => {
                timeout = Some(positive_u64(next(args, &mut index, value)?, value)?)
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
    Ok((
        repository.unwrap_or(repository_root()?).canonicalize()?,
        job_id.ok_or("job id is required")?,
        timeout,
    ))
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
