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
        detach_stdio_from_children();
    }
    command.spawn()?;
    Ok(())
}

/// Stop the detached executor from inheriting the submitter's stdio handles.
///
/// A shell (or an agent harness) that pipes `submit`'s output gives it an *inheritable* stdout
/// handle. `CreateProcessW` passes every inheritable handle to the child even when the child's
/// own stdio is redirected to NUL, so the executor would hold the submitter's pipe open and a
/// `submit | tail` pipeline would never see EOF. Clearing the inherit flag on our own stdio
/// before the spawn is what makes `submit` return when its output is piped.
#[cfg(windows)]
fn detach_stdio_from_children() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    for handle in [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ] {
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
    }
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

    #[cfg(windows)]
    #[test]
    fn detach_stdio_clears_the_inherit_flag() {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{
            GetHandleInformation, HANDLE_FLAG_INHERIT, SetHandleInformation,
        };
        let stdout = std::io::stdout().as_raw_handle();
        let mut original = 0_u32;
        assert_ne!(unsafe { GetHandleInformation(stdout, &mut original) }, 0);
        // Make the handle inheritable first, so the test fails if the helper does nothing.
        unsafe { SetHandleInformation(stdout, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        detach_stdio_from_children();
        let mut flags = 0_u32;
        assert_ne!(unsafe { GetHandleInformation(stdout, &mut flags) }, 0);
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        // Restore the original flag so this test does not leak process-global state.
        unsafe {
            SetHandleInformation(stdout, HANDLE_FLAG_INHERIT, original & HANDLE_FLAG_INHERIT);
        }
    }
}
