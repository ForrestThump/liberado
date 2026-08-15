//! User-context comparison worker and installation support.

use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use chrono::Utc;
use liberado_common::path::child_process_path;
use liberado_common::process::std_command;
use sha2::{Digest, Sha256};

use crate::contract::{ComparisonReport, FailureClass, JobEvent, JobStatus};
use crate::journal::JobStore;
use crate::{engine, transport};

const WORKER_USAGE: &str = "usage: liberado-harness-worker --config <worker-policy.json> [--once]";

pub fn run_command(args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    let mut config = None;
    let mut once = false;
    let mut args = args;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--config" => {
                config = Some(PathBuf::from(
                    args.next().ok_or("--config requires a path")?,
                ));
            }
            "--once" => once = true,
            "-h" | "--help" => {
                println!("{WORKER_USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown worker argument: {other}").into()),
        }
    }
    let config = config.ok_or(WORKER_USAGE)?;
    run(&config, once)
}

/// Persist an early background-worker failure when the policy path can be recovered from argv.
pub fn record_bootstrap_failure(args: impl Iterator<Item = String>, message: &str) {
    let arguments: Vec<_> = args.collect();
    if let Some(index) = arguments.iter().position(|argument| argument == "--config")
        && let Some(path) = arguments.get(index + 1)
    {
        let _ = append_host_log(Path::new(path), "ERROR", message);
    }
}

pub fn run(policy_path: &Path, once: bool) -> Result<(), Box<dyn Error>> {
    let policy_path = policy_path.canonicalize()?;
    let policy = transport::load_policy(&policy_path)?;
    let _instance = WorkerInstance::acquire(&policy_path)?;
    append_host_log(
        &policy_path,
        "INFO",
        &format!(
            "worker started; pid={}; repositories={}; once={once}",
            std::process::id(),
            policy.repositories.len()
        ),
    )?;
    let (wake_send, wake_receive) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = wake_send.send(event);
    })?;
    for repository in &policy.repositories {
        let root = JobStore::for_repository(repository).root().to_path_buf();
        fs::create_dir_all(&root)?;
        notify::Watcher::watch(&mut watcher, &root, notify::RecursiveMode::Recursive)?;
    }
    loop {
        let mut found = false;
        for repository in &policy.repositories {
            let store = JobStore::for_repository(repository);
            for job_id in store.accepted_jobs()? {
                found = true;
                let state = store.load_state(&job_id)?;
                if state.status != JobStatus::Accepted {
                    match store.acquire_lease(&job_id) {
                        Ok(_lease) => {
                            let message = format!(
                                "worker restarted after an interrupted comparison at phase {}; manual retry required",
                                state.phase
                            );
                            append_host_log(
                                &policy_path,
                                "ERROR",
                                &format!("job {job_id} marked failed: {message}"),
                            )?;
                            record_worker_failure(&store, &job_id, message)?;
                        }
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            // Another healthy worker owns this in-flight job.
                        }
                        Err(error) => return Err(error.into()),
                    }
                    continue;
                }
                let execution = catch_unwind(AssertUnwindSafe(|| {
                    engine::execute(&store, &job_id, &policy)
                }));
                match execution {
                    Ok(Ok(report)) => {
                        eprintln!("job {} finished as {:?}", report.job_id, report.status);
                        append_host_log(
                            &policy_path,
                            "INFO",
                            &format!("job {} finished as {:?}", report.job_id, report.status),
                        )?;
                    }
                    Ok(Err(error))
                        if error
                            .downcast_ref::<io::Error>()
                            .is_some_and(|value| value.kind() == io::ErrorKind::AlreadyExists) =>
                    {
                        // Another healthy worker owns this job lease.
                    }
                    Ok(Err(error)) => {
                        eprintln!("job {job_id} worker failure: {error}");
                        append_host_log(
                            &policy_path,
                            "ERROR",
                            &format!("job {job_id} worker failure: {error}"),
                        )?;
                        record_worker_failure(&store, &job_id, error.to_string())?;
                    }
                    Err(panic) => {
                        let message = format!(
                            "worker panic while executing comparison: {}",
                            panic_message(panic)
                        );
                        eprintln!("job {job_id} worker failure: {message}");
                        append_host_log(
                            &policy_path,
                            "ERROR",
                            &format!("job {job_id} worker failure: {message}"),
                        )?;
                        record_worker_failure(&store, &job_id, message)?;
                    }
                }
            }
        }
        if once {
            append_host_log(&policy_path, "INFO", "worker stopped after one scan")?;
            return Ok(());
        }
        if !found {
            // Filesystem events are the normal wake hook. The timeout is only a recovery check for
            // missed or coalesced operating-system notifications.
            let _ = wake_receive
                .recv_timeout(Duration::from_millis(policy.poll_interval_ms.max(30_000)));
        }
    }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = panic.downcast_ref::<String>() {
        return message.clone();
    }
    "panic payload was not a string".to_string()
}

fn append_host_log(policy_path: &Path, level: &str, message: &str) -> io::Result<()> {
    let path = policy_path.with_extension("log");
    let mut log = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(log, "{} {level} {message}", Utc::now().to_rfc3339())?;
    log.flush()
}

pub fn install(repository: &Path, worker_binary: Option<&Path>) -> Result<PathBuf, Box<dyn Error>> {
    let repository = repository.canonicalize()?;
    let policy_path = transport::write_default_policy(&repository, false)?;
    let worker_binary = match worker_binary {
        Some(path) => preferred_background_binary(&path.canonicalize()?),
        None => sibling_worker_binary()?,
    };
    if !worker_binary.is_file() {
        return Err(format!(
            "worker binary does not exist: {}. Build or install liberado-harness-worker first",
            worker_binary.display()
        )
        .into());
    }
    let installed_binary = install_worker_binary(&repository, &worker_binary)?;
    install_platform_worker(&installed_binary, &policy_path)?;
    Ok(policy_path)
}

pub fn start(policy_path: &Path, worker_binary: Option<&Path>) -> Result<(), Box<dyn Error>> {
    let worker_binary = match worker_binary {
        Some(path) => path.canonicalize()?,
        None => sibling_worker_binary()?,
    };
    if !worker_binary.is_file() {
        return Err(format!("worker binary does not exist: {}", worker_binary.display()).into());
    }
    let mut command = std_command(&worker_binary);
    command
        .arg("--config")
        .arg(policy_path.canonicalize()?)
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

fn record_worker_failure(
    store: &JobStore,
    job_id: &crate::contract::JobId,
    message: String,
) -> Result<(), Box<dyn Error>> {
    let mut state = store.load_state(job_id)?;
    if state.status.is_terminal() {
        return Ok(());
    }
    state.revision += 1;
    state.status = JobStatus::Failed;
    state.phase = "worker_failure".to_string();
    state.updated_at = Utc::now();
    state.failure_class = Some(FailureClass::HostInfrastructureFailure);
    state.message = Some(message.clone());
    store.write_state(&state)?;
    store.append_job_event(
        job_id,
        &JobEvent {
            sequence: state.revision,
            at: state.updated_at,
            status: state.status,
            phase: state.phase.clone(),
            message: message.clone(),
        },
    )?;
    let spec = store.load_spec(job_id)?;
    store.write_report(&ComparisonReport {
        version: 1,
        job_id: job_id.clone(),
        experiment_id: spec.experiment_id,
        status: JobStatus::Failed,
        failure_class: Some(FailureClass::HostInfrastructureFailure),
        base_commit: None,
        started_at: state.updated_at,
        finished_at: Utc::now(),
        harnesses: Default::default(),
        diagnostics: vec![message],
        artifact_root: store.job_root(job_id).join("artifacts"),
    })?;
    Ok(())
}

fn sibling_worker_binary() -> Result<PathBuf, Box<dyn Error>> {
    let current = std::env::current_exe()?;
    Ok(current.with_file_name(if cfg!(windows) {
        "liberado-harness-worker-background.exe"
    } else {
        "liberado-harness-worker"
    }))
}

fn preferred_background_binary(binary: &Path) -> PathBuf {
    if cfg!(windows)
        && binary
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("liberado-harness-worker.exe"))
    {
        let background = binary.with_file_name("liberado-harness-worker-background.exe");
        if background.is_file() {
            return background;
        }
    }
    binary.to_path_buf()
}

fn install_worker_binary(repository: &Path, source: &Path) -> io::Result<PathBuf> {
    let bytes = fs::read(source)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let extension = source
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy()))
        .unwrap_or_default();
    let destination = repository.join(".liberado/bin").join(format!(
        "liberado-harness-worker-background-{}{}",
        &digest[..16],
        extension
    ));
    if destination.is_file() {
        return Ok(destination);
    }
    fs::create_dir_all(
        destination
            .parent()
            .expect("worker destination has a parent"),
    )?;
    let temporary = destination.with_extension(format!(
        "{}tmp-{}",
        destination
            .extension()
            .map(|value| format!("{}.", value.to_string_lossy()))
            .unwrap_or_default(),
        std::process::id()
    ));
    fs::write(&temporary, bytes)?;
    match fs::rename(&temporary, &destination) {
        Ok(()) => Ok(destination),
        Err(_) if destination.is_file() => {
            fs::remove_file(temporary)?;
            Ok(destination)
        }
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(error)
        }
    }
}

#[cfg(windows)]
fn install_platform_worker(binary: &Path, policy: &Path) -> Result<(), Box<dyn Error>> {
    let command = windows_startup_command(&child_process_path(binary), &child_process_path(policy));
    set_user_startup_value("Liberado Harness Worker", &command)?;
    start(policy, Some(binary))
}

#[cfg(windows)]
fn windows_startup_command(binary: &Path, policy: &Path) -> String {
    format!("\"{}\" --config \"{}\"", binary.display(), policy.display())
}

#[cfg(windows)]
fn set_user_startup_value(name: &str, value: &str) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegSetValueExW,
    };

    let wide = |text: &str| {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let name = wide(name);
    let value = wide(value);
    let mut key = std::ptr::null_mut();
    let create_result = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if create_result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(create_result as i32));
    }
    let set_result = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * size_of::<u16>()) as u32,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if set_result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(set_result as i32));
    }
    Ok(())
}

#[cfg(not(windows))]
fn install_platform_worker(_binary: &Path, _policy: &Path) -> Result<(), Box<dyn Error>> {
    Err("automatic worker installation is currently implemented for Windows; use your user service manager on this platform".into())
}

#[derive(Debug)]
struct WorkerInstance {
    path: PathBuf,
}

impl WorkerInstance {
    fn acquire(policy_path: &Path) -> io::Result<Self> {
        let path = policy_path.with_extension("worker.lock");
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                writeln!(file, "pid={}", std::process::id())?;
                file.sync_all()?;
                Ok(Self { path })
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let pid = fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| {
                        text.lines()
                            .find_map(|line| line.strip_prefix("pid="))
                            .and_then(|value| value.parse::<u32>().ok())
                    })
                    .unwrap_or(0);
                if pid != 0 && process_is_alive(pid) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("harness worker is already running as process {pid}"),
                    ));
                }
                fs::remove_file(&path)?;
                Self::acquire(policy_path)
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for WorkerInstance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut code = 0_u32;
    let ok = unsafe { GetExitCodeProcess(handle, &mut code) } != 0;
    unsafe { CloseHandle(handle) };
    ok && code == STILL_ACTIVE as u32
}

#[cfg(not(windows))]
fn process_is_alive(pid: u32) -> bool {
    std_command("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_instance_lock_is_exclusive_and_released() {
        let temp = tempfile::tempdir().unwrap();
        let policy = temp.path().join("worker.json");
        fs::write(&policy, "{}").unwrap();
        let first = WorkerInstance::acquire(&policy).unwrap();
        assert_eq!(
            WorkerInstance::acquire(&policy).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        drop(first);
        WorkerInstance::acquire(&policy).unwrap();
    }

    #[test]
    fn background_failures_use_the_predictable_policy_log() {
        let temp = tempfile::tempdir().unwrap();
        let policy = temp.path().join("harness-worker.json");
        record_bootstrap_failure(
            vec![
                "--config".to_string(),
                policy.to_string_lossy().into_owned(),
            ]
            .into_iter(),
            "startup failed",
        );
        let log = fs::read_to_string(policy.with_extension("log")).unwrap();
        assert!(log.contains("ERROR startup failed"));
    }

    #[test]
    fn panic_payloads_are_rendered_without_panicking_again() {
        assert_eq!(
            panic_message(Box::new("runtime missing")),
            "runtime missing"
        );
        assert_eq!(
            panic_message(Box::new(String::from("runtime missing"))),
            "runtime missing"
        );
        assert_eq!(
            panic_message(Box::new(42_u32)),
            "panic payload was not a string"
        );
    }

    #[cfg(windows)]
    #[test]
    fn installer_prefers_the_windowless_sibling_when_it_exists() {
        let temp = tempfile::tempdir().unwrap();
        let console = temp.path().join("liberado-harness-worker.exe");
        let background = temp.path().join("liberado-harness-worker-background.exe");
        fs::write(&console, []).unwrap();
        fs::write(&background, []).unwrap();
        assert_eq!(preferred_background_binary(&console), background);
    }

    #[test]
    fn installed_worker_binary_is_content_addressed_runtime_state() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join(if cfg!(windows) {
            "liberado-harness-worker-background.exe"
        } else {
            "liberado-harness-worker-background"
        });
        fs::write(&source, b"worker-v1").unwrap();
        let installed = install_worker_binary(temp.path(), &source).unwrap();
        assert!(installed.starts_with(temp.path().join(".liberado/bin")));
        assert_eq!(fs::read(&installed).unwrap(), b"worker-v1");
        assert_eq!(
            install_worker_binary(temp.path(), &source).unwrap(),
            installed
        );
    }

    #[cfg(windows)]
    #[test]
    fn startup_command_quotes_paths_without_literal_escape_characters() {
        let command = windows_startup_command(
            Path::new("C:/Program Files/Liberado/worker.exe"),
            Path::new("C:/Users/Test User/worker.json"),
        );
        assert_eq!(
            command,
            "\"C:/Program Files/Liberado/worker.exe\" --config \"C:/Users/Test User/worker.json\""
        );
        assert!(!command.contains("\\\""));
    }
}
