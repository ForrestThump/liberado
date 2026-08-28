use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::DevelopmentConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevAction {
    StartDaemon,
    StopDaemon,
    StartWebui,
    StopWebui,
    Status,
    Tui,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DevOptions {
    pub vault: Option<PathBuf>,
    pub build: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProcessRecord {
    pid: u32,
    program: String,
    started_unix_secs: u64,
}

pub fn run_dev(
    repository: &Path,
    config: &DevelopmentConfig,
    action: DevAction,
    options: &DevOptions,
) -> Result<(), String> {
    match action {
        DevAction::StartDaemon => start_daemon(repository, config, options),
        DevAction::StopDaemon => stop_process(repository, "daemon"),
        DevAction::StartWebui => start_webui(repository, config),
        DevAction::StopWebui => stop_process(repository, "webui-dev"),
        DevAction::Status => status(repository),
        DevAction::Tui => run_tui(repository, config),
    }
}

fn start_daemon(
    repository: &Path,
    config: &DevelopmentConfig,
    options: &DevOptions,
) -> Result<(), String> {
    let status_url = format!("{}/api/status", config.daemon_url.trim_end_matches('/'));
    if is_ready(&status_url) {
        println!("Daemon is already ready at {}", config.daemon_url);
        return Ok(());
    }
    if options.build {
        run_status(
            repository,
            "cargo",
            &["build", "--locked", "--bin", "liberado"],
        )?;
    }
    let vault = resolve_vault(repository, options.vault.as_deref())?;
    let executable = current_liberado_executable(repository)?;
    let args = vec!["serve".to_string(), vault.display().to_string()];
    spawn_detached(
        repository,
        "daemon",
        &executable,
        &args,
        &[("LIBERADO_PORT", config.daemon_port.to_string())],
    )?;
    wait_ready(
        repository,
        "daemon",
        &status_url,
        config.readiness_timeout_secs,
    )
}

fn start_webui(repository: &Path, config: &DevelopmentConfig) -> Result<(), String> {
    let args = vec![
        "serve".into(),
        "-p".into(),
        "liberado-webui".into(),
        "--platform".into(),
        "web".into(),
        "--addr".into(),
        "0.0.0.0".into(),
        "--port".into(),
        config.webui_dev_port.to_string(),
    ];
    spawn_detached(repository, "webui-dev", Path::new("dx"), &args, &[])?;
    wait_ready(
        repository,
        "webui-dev",
        &format!("http://127.0.0.1:{}", config.webui_dev_port),
        config.readiness_timeout_secs,
    )
}

fn run_tui(repository: &Path, config: &DevelopmentConfig) -> Result<(), String> {
    let status = liberado_common::process::std_command("cargo")
        .args(["run", "--locked", "-p", "liberado-tui"])
        .env("LIBERADO_SERVER", &config.daemon_url)
        .current_dir(repository)
        .status()
        .map_err(|error| format!("start TUI: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("TUI exited with {status}"))
    }
}

fn spawn_detached(
    repository: &Path,
    name: &str,
    program: &Path,
    args: &[String],
    envs: &[(&str, String)],
) -> Result<(), String> {
    let state_dir = repository.join(".liberado");
    std::fs::create_dir_all(&state_dir)
        .map_err(|error| format!("create {}: {error}", state_dir.display()))?;
    let stdout = File::create(state_dir.join(format!("{name}.log")))
        .map_err(|error| format!("create {name} log: {error}"))?;
    let stderr = File::create(state_dir.join(format!("{name}.err.log")))
        .map_err(|error| format!("create {name} error log: {error}"))?;
    let mut command = liberado_common::process::std_command(program);
    command
        .args(args)
        .envs(envs.iter().map(|(name, value)| (*name, value)))
        .current_dir(repository)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let child = spawn_platform(&mut command)
        .map_err(|error| format!("start {}: {error}", program.display()))?;
    let record = ProcessRecord {
        pid: child.id(),
        program: program.display().to_string(),
        started_unix_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let json = serde_json::to_vec_pretty(&record)
        .map_err(|error| format!("serialize process record: {error}"))?;
    std::fs::write(process_file(repository, name), json)
        .map_err(|error| format!("write {name} process record: {error}"))?;
    println!("Started {name} (PID {})", record.pid);
    Ok(())
}

#[cfg(windows)]
fn spawn_platform(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    match command.spawn() {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            command.spawn()
        }
        result => result,
    }
}

#[cfg(not(windows))]
fn spawn_platform(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    command.spawn()
}

fn stop_process(repository: &Path, name: &str) -> Result<(), String> {
    let path = process_file(repository, name);
    if !path.is_file() {
        println!("No {name} process record; nothing to stop.");
        return Ok(());
    }
    let record: ProcessRecord = serde_json::from_slice(
        &std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if process_matches(&record)? {
        kill_process(record.pid)?;
        println!("Stopped {name} (PID {}).", record.pid);
    } else {
        println!(
            "PID {} is absent or does not match {}; leaving it alone.",
            record.pid, record.program
        );
    }
    std::fs::remove_file(&path).map_err(|error| format!("remove {}: {error}", path.display()))
}

fn status(repository: &Path) -> Result<(), String> {
    for name in ["daemon", "webui-dev"] {
        let path = process_file(repository, name);
        if !path.is_file() {
            println!("{name}: stopped");
            continue;
        }
        let record: ProcessRecord = serde_json::from_slice(
            &std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?,
        )
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
        println!(
            "{name}: {} (PID {})",
            if process_matches(&record)? {
                "running"
            } else {
                "stale record"
            },
            record.pid
        );
    }
    Ok(())
}

#[cfg(windows)]
fn process_matches(record: &ProcessRecord) -> Result<bool, String> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, record.pid) };
    if handle.is_null() {
        return Ok(false);
    }
    let mut buffer = vec![0_u16; 32_768];
    let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    let queried =
        unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) } != 0;
    unsafe { CloseHandle(handle) };
    if !queried {
        return Ok(false);
    }
    let actual = PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..usize::try_from(length).unwrap_or(0)],
    ));
    Ok(actual
        .file_name()
        .zip(Path::new(&record.program).file_name())
        .is_some_and(|(actual, expected)| {
            actual
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected.to_string_lossy())
        }))
}

#[cfg(not(windows))]
fn process_matches(record: &ProcessRecord) -> Result<bool, String> {
    let proc_exe = PathBuf::from(format!("/proc/{}/exe", record.pid));
    let Ok(actual) = std::fs::read_link(proc_exe) else {
        return Ok(false);
    };
    let expected = Path::new(&record.program)
        .file_name()
        .and_then(|name| name.to_str());
    Ok(actual.file_name().and_then(|name| name.to_str()) == expected)
}

#[cfg(windows)]
fn kill_process(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        return Err(format!(
            "open PID {pid} for termination: {}",
            std::io::Error::last_os_error()
        ));
    }
    let terminated = unsafe { TerminateProcess(handle, 1) } != 0;
    let error = std::io::Error::last_os_error();
    unsafe { CloseHandle(handle) };
    if terminated {
        Ok(())
    } else {
        Err(format!("terminate PID {pid}: {error}"))
    }
}

#[cfg(not(windows))]
fn kill_process(pid: u32) -> Result<(), String> {
    run_status(Path::new("."), "kill", &["-TERM", &pid.to_string()])
}

fn resolve_vault(repository: &Path, explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return require_directory(path);
    }
    if let Some(path) = std::env::var_os("LIBERADO_VAULT").filter(|value| !value.is_empty()) {
        return require_directory(Path::new(&path));
    }
    let config_dir = liberado_config::config_dir().unwrap_or_else(|| repository.join("config"));
    let topology_path = config_dir.join("topology.toml");
    let source = std::fs::read_to_string(&topology_path)
        .map_err(|error| format!("read {} for vault_path: {error}", topology_path.display()))?;
    let topology: liberado_config_loader::Topology = toml::from_str(&source)
        .map_err(|error| format!("parse {}: {error}", topology_path.display()))?;
    require_directory(&topology.vault_path)
}

fn require_directory(path: &Path) -> Result<PathBuf, String> {
    if path.is_dir() {
        Ok(path.to_path_buf())
    } else {
        Err(format!(
            "vault directory does not exist: {}",
            path.display()
        ))
    }
}

fn current_liberado_executable(repository: &Path) -> Result<PathBuf, String> {
    let current =
        std::env::current_exe().map_err(|error| format!("resolve current executable: {error}"))?;
    let name = current
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if name == "liberado" {
        return Ok(current);
    }
    let candidate = repository
        .join("target")
        .join("debug")
        .join(format!("liberado{}", std::env::consts::EXE_SUFFIX));
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err("liberado binary not found; run `cargo build --bin liberado` or pass --build".into())
    }
}

fn wait_ready(repository: &Path, name: &str, url: &str, timeout_secs: u64) -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    while std::time::Instant::now() < deadline {
        let path = process_file(repository, name);
        let record: ProcessRecord = serde_json::from_slice(
            &std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?,
        )
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
        if !process_matches(&record)? {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove stale {}: {error}", path.display()))?;
            return Err(format!(
                "{name} exited before {url} became ready; see .liberado/{name}.err.log"
            ));
        }
        if is_ready(url) {
            println!("Ready: {url}");
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(format!("{url} did not become ready within {timeout_secs}s"))
}

fn is_ready(url: &str) -> bool {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(url).send())
        .is_ok_and(|response| response.status().is_success())
}

fn process_file(repository: &Path, name: &str) -> PathBuf {
    repository
        .join(".liberado")
        .join(format!("{name}.process.json"))
}

fn run_status(cwd: &Path, program: &str, args: &[&str]) -> Result<(), String> {
    let status = liberado_common::process::std_command(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .map_err(|error| format!("run {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} failed with {status}", args.join(" ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_record_round_trips() {
        let record = ProcessRecord {
            pid: 42,
            program: "liberado".into(),
            started_unix_secs: 7,
        };
        let json = serde_json::to_string(&record).unwrap();
        let decoded: ProcessRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.pid, 42);
        assert_eq!(decoded.program, "liberado");
    }

    #[test]
    fn process_files_stay_in_gitignored_state_dir() {
        assert_eq!(
            process_file(Path::new("repo"), "daemon"),
            Path::new("repo/.liberado/daemon.process.json")
        );
    }

    #[test]
    fn readiness_fails_immediately_when_the_child_has_exited() {
        let repository = tempfile::tempdir().unwrap();
        std::fs::create_dir(repository.path().join(".liberado")).unwrap();
        let path = process_file(repository.path(), "daemon");
        std::fs::write(
            &path,
            serde_json::to_vec(&ProcessRecord {
                pid: u32::MAX,
                program: "liberado-never-running".into(),
                started_unix_secs: 7,
            })
            .unwrap(),
        )
        .unwrap();

        let error = wait_ready(repository.path(), "daemon", "http://127.0.0.1:9", 30)
            .expect_err("an exited process must fail before the HTTP timeout");

        assert!(error.contains("exited before"), "{error}");
        assert!(!path.exists(), "the stale process record must be removed");
    }
}
