//! Per-job detached executor.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use liberado_common::process::std_command;

use crate::contract::{JobId, JobStatus};
use crate::journal::{JobStore, RunnerLock};
use crate::{engine, transport};

const WORKER_USAGE: &str = "usage: liberado-harness-worker run-job <job-id> [--source <repo>]";

pub fn run_command(args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    let mut args = args;
    let command = args.next().ok_or(WORKER_USAGE)?;
    match command.as_str() {
        "run-job" => {
            let mut repository = None;
            let mut job_id = None;
            while let Some(argument) = args.next() {
                match argument.as_str() {
                    "--source" => {
                        repository = Some(PathBuf::from(
                            args.next().ok_or("--source requires a path")?,
                        ));
                    }
                    "-h" | "--help" => {
                        println!("{WORKER_USAGE}");
                        return Ok(());
                    }
                    other => {
                        if job_id.is_some() {
                            return Err(format!("unknown worker argument: {other}").into());
                        }
                        job_id = Some(JobId::parse(other)?);
                    }
                }
            }
            let repository = repository.ok_or("run-job requires --source <repo>")?;
            let job_id = job_id.ok_or("run-job requires <job-id>")?;
            run_job(&repository, &job_id)
        }
        "-h" | "--help" => {
            println!("{WORKER_USAGE}");
            Ok(())
        }
        other => Err(format!("unknown worker command: {other}").into()),
    }
}

/// Run one job to a terminal state. The executor inherits the submitter's environment, so the
/// credential alias resolves from the process environment — no HKCU read, no daemon.
pub fn run_job(repository: &Path, job_id: &JobId) -> Result<(), Box<dyn Error>> {
    let repository = repository.canonicalize()?;
    let store = JobStore::for_repository(&repository);
    let _lock = match RunnerLock::acquire(&store) {
        Ok(lock) => lock,
        Err(error) => {
            store.mark_failed(job_id, error.to_string())?;
            return Err(error.into());
        }
    };
    let policy = transport::load_policy(&transport::policy_path(&repository))?;
    let report = engine::execute(&store, job_id, &policy)?;
    if report.status == JobStatus::Succeeded {
        Ok(())
    } else {
        Err(format!("job {job_id} finished as {:?}", report.status).into())
    }
}

/// Spawn a detached executor for a freshly submitted job and return immediately. Non-blocking is a
/// property of process spawning, not of a service.
pub fn spawn_executor(repository: &Path, job_id: &JobId) -> Result<(), Box<dyn Error>> {
    let binary = executor_binary()?;
    let mut command = std_command(&binary);
    command
        .arg("run-job")
        .arg(&job_id.0)
        .arg("--source")
        .arg(repository)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    command.spawn()?;
    Ok(())
}

fn executor_binary() -> Result<PathBuf, Box<dyn Error>> {
    let current = std::env::current_exe()?;
    Ok(current.with_file_name(if cfg!(windows) {
        "liberado-harness-worker.exe"
    } else {
        "liberado-harness-worker"
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_rejects_unknown_commands() {
        let error = run_command(["install".to_string()].into_iter()).unwrap_err();
        assert!(error.to_string().contains("unknown worker command"));
    }

    #[test]
    fn run_job_requires_a_source_repository() {
        let error = run_command(["run-job".to_string()].into_iter()).unwrap_err();
        assert!(error.to_string().contains("--source"));
    }
}
