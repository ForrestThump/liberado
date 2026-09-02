//! Argument adapter for durable comparison jobs.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::contract::*;
use crate::journal::{JobStore, RunnerLock};
use crate::{preflight, repository_root, transport, worker};

mod harnesses;
use harnesses::{doctor_spec, print_doctor_report, queue_job};

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
     liberado coder compare submit --task <file> [pins] [--harnesses <ids>] [--wait] [--no-spawn]\n  \
     liberado coder compare doctor --task <file> [pins] [--harnesses <ids>]\n  \
     liberado coder compare status <job-id> [--source <repo>]\n  \
     liberado coder compare await <job-id> [--timeout-secs <n>] [--stall-secs <n>] [--source <repo>]\n  \
     liberado coder compare cancel <job-id> [--source <repo>]\n  \
     liberado coder compare report <job-id> [--json] [--source <repo>]"
}

/// Parsed `compare submit` / `compare doctor` flags. Defaults match the CLI's historical
/// values, so a bare `submit --task <file>` behaves exactly as before.
#[derive(Debug)]
struct SubmitArgs {
    repository: Option<PathBuf>,
    task: Option<PathBuf>,
    commit: String,
    provider: String,
    model: String,
    base_url: String,
    credential_alias: String,
    thinking: String,
    limits: ResourceLimits,
    max_turns: u32,
    task_aware_context: bool,
    acceptance_overlay: Option<PathBuf>,
    harnesses: Option<Vec<String>>,
    liberado_bin: Option<PathBuf>,
    pi_bin: Option<PathBuf>,
    hermes_bin: Option<PathBuf>,
    deep_agents_bin: Option<PathBuf>,
    hermes_git_sha: Option<String>,
    deep_agents_git_sha: Option<String>,
    hypothesis: Option<String>,
    variable: Option<String>,
    wait: bool,
    no_spawn: bool,
    wait_timeout: Option<u64>,
    help: bool,
}

impl Default for SubmitArgs {
    fn default() -> Self {
        Self {
            repository: None,
            task: None,
            commit: "main".to_string(),
            provider: "openrouter".to_string(),
            model: "deepseek/deepseek-v4-flash".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            credential_alias: "openrouter-default".to_string(),
            thinking: "high".to_string(),
            limits: ResourceLimits::default(),
            max_turns: 400,
            task_aware_context: false,
            acceptance_overlay: None,
            harnesses: None,
            liberado_bin: None,
            pi_bin: None,
            hermes_bin: None,
            deep_agents_bin: None,
            hermes_git_sha: None,
            deep_agents_git_sha: None,
            hypothesis: None,
            variable: None,
            wait: false,
            no_spawn: false,
            wait_timeout: None,
            help: false,
        }
    }
}

/// One flag's parser: consumes the flag's value argument (via `next`) and records it on
/// `SubmitArgs`. `index` is left pointing at the value the flag consumed, and the parse loop
/// advances past it.
type FlagHandler = fn(&[String], &mut usize, &mut SubmitArgs) -> Result<(), Box<dyn Error>>;

macro_rules! string_flag {
    ($name:ident, $flag:literal, $field:ident) => {
        fn $name(
            args: &[String],
            index: &mut usize,
            parsed: &mut SubmitArgs,
        ) -> Result<(), Box<dyn Error>> {
            parsed.$field = next(args, index, $flag)?.to_string();
            Ok(())
        }
    };
}

macro_rules! opt_string_flag {
    ($name:ident, $flag:literal, $field:ident) => {
        fn $name(
            args: &[String],
            index: &mut usize,
            parsed: &mut SubmitArgs,
        ) -> Result<(), Box<dyn Error>> {
            parsed.$field = Some(next(args, index, $flag)?.to_string());
            Ok(())
        }
    };
}

macro_rules! path_flag {
    ($name:ident, $flag:literal, $field:ident) => {
        fn $name(
            args: &[String],
            index: &mut usize,
            parsed: &mut SubmitArgs,
        ) -> Result<(), Box<dyn Error>> {
            parsed.$field = Some(PathBuf::from(next(args, index, $flag)?));
            Ok(())
        }
    };
}

macro_rules! bool_flag {
    ($name:ident, $flag:literal, $field:ident) => {
        fn $name(
            _args: &[String],
            _index: &mut usize,
            parsed: &mut SubmitArgs,
        ) -> Result<(), Box<dyn Error>> {
            parsed.$field = true;
            Ok(())
        }
    };
}

string_flag!(commit_flag, "--commit", commit);
string_flag!(provider_flag, "--provider", provider);
string_flag!(model_flag, "--model", model);
string_flag!(base_url_flag, "--base-url", base_url);
string_flag!(credential_flag, "--credential", credential_alias);
string_flag!(thinking_flag, "--thinking", thinking);
opt_string_flag!(hypothesis_flag, "--hypothesis", hypothesis);
opt_string_flag!(variable_flag, "--variable", variable);

path_flag!(source_flag, "--source", repository);
path_flag!(task_flag, "--task", task);
path_flag!(
    acceptance_overlay_flag,
    "--acceptance-overlay",
    acceptance_overlay
);
path_flag!(liberado_bin_flag, "--liberado-bin", liberado_bin);
path_flag!(pi_bin_flag, "--pi-bin", pi_bin);
path_flag!(hermes_bin_flag, "--hermes-bin", hermes_bin);
path_flag!(deep_agents_bin_flag, "--deep-agents-bin", deep_agents_bin);
opt_string_flag!(hermes_git_sha_flag, "--hermes-git-sha", hermes_git_sha);
opt_string_flag!(
    deep_agents_git_sha_flag,
    "--deep-agents-git-sha",
    deep_agents_git_sha
);

fn harnesses_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut SubmitArgs,
) -> Result<(), Box<dyn Error>> {
    let raw = next(args, index, "--harnesses")?;
    let ids: Vec<String> = raw
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    if ids.is_empty() {
        return Err("--harnesses must name at least one harness".into());
    }
    parsed.harnesses = Some(ids);
    Ok(())
}

bool_flag!(
    task_aware_context_flag,
    "--task-aware-context",
    task_aware_context
);
bool_flag!(wait_flag, "--wait", wait);
bool_flag!(no_spawn_flag, "--no-spawn", no_spawn);

fn max_turns_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut SubmitArgs,
) -> Result<(), Box<dyn Error>> {
    parsed.max_turns = positive_u32(next(args, index, "--max-turns")?, "--max-turns")?;
    Ok(())
}

fn timeout_secs_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut SubmitArgs,
) -> Result<(), Box<dyn Error>> {
    parsed.wait_timeout = Some(positive_u64(
        next(args, index, "--timeout-secs")?,
        "--timeout-secs",
    )?);
    Ok(())
}

fn compile_timeout_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut SubmitArgs,
) -> Result<(), Box<dyn Error>> {
    parsed.limits.compile_timeout_secs = positive_u64(
        next(args, index, "--compile-timeout-secs")?,
        "--compile-timeout-secs",
    )?;
    Ok(())
}

fn run_timeout_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut SubmitArgs,
) -> Result<(), Box<dyn Error>> {
    parsed.limits.run_timeout_secs = positive_u64(
        next(args, index, "--run-timeout-secs")?,
        "--run-timeout-secs",
    )?;
    Ok(())
}

fn verifier_repair_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut SubmitArgs,
) -> Result<(), Box<dyn Error>> {
    parsed.limits.verifier_repair_attempts = next(args, index, "--verifier-repair-attempts")?
        .parse()
        .map_err(|_| "--verifier-repair-attempts must be a non-negative integer")?;
    Ok(())
}

fn minimum_free_gib_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut SubmitArgs,
) -> Result<(), Box<dyn Error>> {
    parsed.limits.minimum_free_bytes = positive_u64(
        next(args, index, "--minimum-free-gib")?,
        "--minimum-free-gib",
    )?
    .saturating_mul(1024 * 1024 * 1024);
    Ok(())
}

/// Flags accepted by both `compare submit` and `compare doctor`.
const COMMON_FLAG_HANDLERS: &[(&str, FlagHandler)] = &[
    ("--source", source_flag),
    ("--task", task_flag),
    ("--commit", commit_flag),
    ("--provider", provider_flag),
    ("--model", model_flag),
    ("--base-url", base_url_flag),
    ("--credential", credential_flag),
    ("--thinking", thinking_flag),
    ("--max-turns", max_turns_flag),
    ("--compile-timeout-secs", compile_timeout_flag),
    ("--run-timeout-secs", run_timeout_flag),
    ("--minimum-free-gib", minimum_free_gib_flag),
    ("--task-aware-context", task_aware_context_flag),
    ("--acceptance-overlay", acceptance_overlay_flag),
    ("--harnesses", harnesses_flag),
    ("--liberado-bin", liberado_bin_flag),
    ("--pi-bin", pi_bin_flag),
    ("--hermes-bin", hermes_bin_flag),
    ("--deep-agents-bin", deep_agents_bin_flag),
    ("--hermes-git-sha", hermes_git_sha_flag),
    ("--deep-agents-git-sha", deep_agents_git_sha_flag),
];

/// Flags `compare submit` accepts on top of the common set (spawn/wait/experiment controls).
const SUBMIT_ONLY_FLAG_HANDLERS: &[(&str, FlagHandler)] = &[
    ("--verifier-repair-attempts", verifier_repair_flag),
    ("--hypothesis", hypothesis_flag),
    ("--variable", variable_flag),
    ("--wait", wait_flag),
    ("--no-spawn", no_spawn_flag),
    ("--timeout-secs", timeout_secs_flag),
];

/// Parse `[flags]` into `SubmitArgs`. `-h`/`--help` short-circuits with `help = true` (the caller
/// prints usage and succeeds). Unknown flags fail with the command-specific message the tests
/// assert (`unknown compare {command} argument`). The common table is consulted first so submit
/// and doctor reject each other's flags exactly as before.
fn parse_flags(
    args: &[String],
    common: &[(&str, FlagHandler)],
    extra: &[(&str, FlagHandler)],
    command: &str,
) -> Result<SubmitArgs, Box<dyn Error>> {
    let mut parsed = SubmitArgs::default();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        if flag == "-h" || flag == "--help" {
            parsed.help = true;
            return Ok(parsed);
        }
        let Some((_, apply)) = common.iter().chain(extra).find(|(name, _)| *name == flag) else {
            return Err(format!("unknown compare {command} argument: {flag}").into());
        };
        apply(args, &mut index, &mut parsed)?;
        index += 1;
    }
    Ok(parsed)
}

/// Resolve the working repository: the explicit `--source`, or the caller's current repository.
fn resolve_repository(parsed: &SubmitArgs) -> Result<PathBuf, Box<dyn Error>> {
    Ok(parsed
        .repository
        .clone()
        .unwrap_or(repository_root()?)
        .canonicalize()?)
}

/// Wait for the queued job and print its terminal status and report path.
fn wait_and_report(
    repository: &Path,
    job_id: &JobId,
    wait_timeout: Option<u64>,
) -> Result<(), Box<dyn Error>> {
    let state = transport::await_terminal(
        repository,
        job_id,
        wait_timeout.map(Duration::from_secs),
        None,
    )?;
    let report_path = JobStore::for_repository(repository)
        .job_root(job_id)
        .join("report.md");
    println!("status={:?}", state.status);
    println!("report={}", report_path.display());
    if state.status != JobStatus::Succeeded {
        return Err(format!("job {} finished as {:?}", job_id, state.status).into());
    }
    Ok(())
}

fn submit(args: &[String]) -> Result<(), Box<dyn Error>> {
    let parsed = parse_flags(
        args,
        COMMON_FLAG_HANDLERS,
        SUBMIT_ONLY_FLAG_HANDLERS,
        "submit",
    )?;
    if parsed.help {
        println!("{}", usage());
        return Ok(());
    }
    let repository = resolve_repository(&parsed)?;
    let spec = queue_job(&parsed, &repository)?;
    println!("{}", spec.job_id);
    println!("experiment_id={}", spec.experiment_id);
    println!("status=accepted");
    if !parsed.no_spawn {
        worker::spawn_executor(&repository, &spec.job_id)?;
    }
    if parsed.wait {
        wait_and_report(&repository, &spec.job_id, parsed.wait_timeout)?;
    }
    Ok(())
}

fn doctor(args: &[String]) -> Result<(), Box<dyn Error>> {
    let parsed = parse_flags(args, COMMON_FLAG_HANDLERS, &[], "doctor")?;
    if parsed.help {
        println!("{}", usage());
        return Ok(());
    }
    let repository = resolve_repository(&parsed)?;
    let spec = doctor_spec(&parsed, &repository)?;
    let policy_path = transport::policy_path(&repository);
    let policy = transport::load_policy(&policy_path).map_err(|error| {
        format!(
            "worker policy is unavailable at {}: {error}",
            policy_path.display()
        )
    })?;
    let report = preflight::run(&spec, &policy)?;
    print_doctor_report(&report.0);
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
#[path = "job_cli_tests.rs"]
mod tests;
