//! Filesystem transport shared by sandboxed clients and the user-context worker.
//!
//! The repository ACL is the transport ACL. A submitter can write only a typed, policy-bounded
//! job. The worker never accepts a shell command or a secret in the request.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use liberado_common::process::std_command;
use sha2::{Digest, Sha256};

use crate::contract::*;
use crate::journal::JobStore;

pub const WORKER_POLICY_FILE: &str = ".liberado/harness-worker.json";

#[derive(Debug, Clone)]
pub struct SubmitOptions {
    pub repository: PathBuf,
    pub base_revision: String,
    pub task_file: PathBuf,
    pub harnesses: Vec<HarnessRequest>,
    pub run_order: Vec<String>,
    pub model: ModelPins,
    pub limits: ResourceLimits,
    pub verifier: VerifierProfile,
    pub task_aware_context: bool,
    pub acceptance_overlay: Option<PathBuf>,
    pub experiment: Option<Experiment>,
}

pub fn submit(options: SubmitOptions) -> Result<JobSpec, Box<dyn Error>> {
    let acceptance_source = options.acceptance_overlay.clone();
    let spec = build_spec(options)?;
    let repository = spec.repository.clone();
    let store = JobStore::for_repository(&repository);
    store.create_with_inputs(&spec, |job_root| {
        fs::write(job_root.join("input/task.txt"), &spec.task.text)?;
        if let Some(source) = acceptance_source.as_deref() {
            let destination = job_root.join("input/acceptance-overlay");
            copy_tree(source, &destination)?;
            let (digest, file_count) = fingerprint_tree(&destination)
                .map_err(|error| io::Error::other(error.to_string()))?;
            let expected = spec.acceptance.as_ref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "acceptance spec is missing")
            })?;
            if digest != expected.sha256 || file_count != expected.file_count {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "acceptance overlay changed while it was being captured",
                ));
            }
        }
        Ok(())
    })?;
    Ok(spec)
}

/// Build and validate an immutable job specification without creating a queued job.
pub fn build_spec(options: SubmitOptions) -> Result<JobSpec, Box<dyn Error>> {
    let repository = options.repository.canonicalize()?;
    let base_revision = resolve_commit(&repository, &options.base_revision)?;
    let task_file = options.task_file.canonicalize()?;
    let task = TaskBundle::new(
        task_file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        fs::read_to_string(&task_file)?,
    )?;
    let acceptance = options
        .acceptance_overlay
        .as_deref()
        .map(fingerprint_tree)
        .transpose()?
        .map(|(sha256, file_count)| AcceptanceBundle {
            directory: PathBuf::from("input/acceptance-overlay"),
            sha256,
            file_count,
        });
    let harnesses = options
        .harnesses
        .into_iter()
        .map(|mut harness| {
            if let Some(binary) = harness.binary.as_deref() {
                harness.binary = Some(binary.canonicalize()?);
            }
            Ok(harness)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let spec = JobSpec {
        version: JOB_SPEC_VERSION,
        job_id: JobId::new(),
        submitted_at: Utc::now(),
        repository: repository.clone(),
        base_revision,
        task,
        harnesses,
        run_order: options.run_order,
        model: options.model,
        limits: options.limits,
        verifier: options.verifier,
        task_aware_context: options.task_aware_context,
        acceptance,
        experiment: options.experiment,
        experiment_id: String::new(),
    }
    .finalize()?;
    Ok(spec)
}

pub(crate) fn verify_captured_inputs(
    spec: &JobSpec,
    job_root: &Path,
) -> Result<(), Box<dyn Error>> {
    let task = fs::read_to_string(job_root.join("input/task.txt"))?;
    if task != spec.task.text || crate::contract::sha256(task.as_bytes()) != spec.task.sha256 {
        return Err("captured task does not match immutable job.json".into());
    }
    if let Some(expected) = &spec.acceptance {
        let (digest, file_count) = fingerprint_tree(&job_root.join(&expected.directory))?;
        if digest != expected.sha256 || file_count != expected.file_count {
            return Err("captured acceptance overlay does not match immutable job.json".into());
        }
    }
    Ok(())
}

fn resolve_commit(repository: &Path, revision: &str) -> Result<String, Box<dyn Error>> {
    let output = std_command("git")
        .arg("-C")
        .arg(repository)
        .args(["rev-parse", &format!("{revision}^{{commit}}")])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "could not resolve comparison revision '{}': {}",
            revision,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn status(repository: &Path, job_id: &JobId) -> io::Result<JobState> {
    let store = JobStore::for_repository(repository);
    store.sweep_dead_lease(job_id)?;
    store.load_state(job_id)
}

pub fn cancel(repository: &Path, job_id: &JobId) -> io::Result<()> {
    let store = JobStore::for_repository(repository);
    let state = store.load_state(job_id)?;
    if state.status.is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("job is already terminal: {:?}", state.status),
        ));
    }
    store.request_cancel(job_id)
}

pub fn report(repository: &Path, job_id: &JobId) -> io::Result<ComparisonReport> {
    JobStore::for_repository(repository).load_report(job_id)
}

/// Wait inside one local process. This does not consume model turns or require a Paseo hook.
///
/// `stall_secs`, when set, exits with a distinct error if neither the event log nor any harness
/// stdout/stderr log has grown in that many seconds.
pub fn await_terminal(
    repository: &Path,
    job_id: &JobId,
    timeout: Option<Duration>,
    stall_secs: Option<u64>,
) -> io::Result<JobState> {
    let store = JobStore::for_repository(repository);
    store.sweep_dead_lease(job_id)?;
    let started = Instant::now();
    let mut revision = None;
    let mut last_progress = progress_mtime(&store.job_root(job_id))?;
    let (wake_send, wake_receive) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = wake_send.send(event);
    })
    .map_err(io::Error::other)?;
    notify::Watcher::watch(
        &mut watcher,
        &store.job_root(job_id),
        notify::RecursiveMode::NonRecursive,
    )
    .map_err(io::Error::other)?;
    loop {
        let state = store.load_state(job_id)?;
        if revision != Some(state.revision) {
            println!(
                "{} {:?} {}",
                state.updated_at.to_rfc3339(),
                state.status,
                state.phase
            );
            revision = Some(state.revision);
        }
        if state.status.is_terminal() {
            return Ok(state);
        }
        if timeout.is_some_and(|limit| started.elapsed() >= limit) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("job {job_id} did not finish before the await timeout"),
            ));
        }
        if let Some(limit) = stall_secs {
            let now = progress_mtime(&store.job_root(job_id))?;
            if now != last_progress {
                last_progress = now;
            } else if last_progress.is_some_and(|at| {
                at.elapsed()
                    .map_err(io::Error::other)
                    .is_ok_and(|age| age >= Duration::from_secs(limit))
            }) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("job {job_id} stalled: no progress for {limit} seconds"),
                ));
            }
        }
        let wait = timeout
            .map(|limit| limit.saturating_sub(started.elapsed()))
            .unwrap_or(Duration::from_secs(30))
            .min(Duration::from_secs(30));
        let _ = wake_receive.recv_timeout(wait);
    }
}

/// The most recent modification time among the files that grow while a comparison makes progress:
/// the event log and each harness's stdout/stderr logs.
fn progress_mtime(job_root: &Path) -> io::Result<Option<std::time::SystemTime>> {
    let mut latest: Option<std::time::SystemTime> = None;
    let mut consider = |path: &Path| {
        if let Ok(metadata) = fs::metadata(path)
            && let Ok(modified) = metadata.modified()
            && latest.is_none_or(|at| modified > at)
        {
            latest = Some(modified);
        }
    };
    consider(&job_root.join("events.jsonl"));
    for base in ["execution/artifacts", "artifacts/harnesses"] {
        let root = job_root.join(base);
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for harness in entries.flatten() {
            let Ok(files) = fs::read_dir(harness.path()) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name();
                let name = name.to_string_lossy();
                if name.ends_with(".stdout.log") || name.ends_with(".stderr.log") {
                    consider(&file.path());
                }
            }
        }
    }
    Ok(latest)
}

pub fn policy_path(repository: &Path) -> PathBuf {
    repository.join(WORKER_POLICY_FILE)
}

pub fn load_policy(path: &Path) -> io::Result<WorkerPolicy> {
    crate::journal::read_json(path)
}

fn fingerprint_tree(root: &Path) -> Result<(String, usize), Box<dyn Error>> {
    if !root.is_dir() {
        return Err(format!("acceptance overlay is not a directory: {}", root.display()).into());
    }
    let files = tree_files(root)?;
    if files.is_empty() {
        return Err("acceptance overlay contains no files".into());
    }
    let mut digest = Sha256::new();
    for (relative, source) in &files {
        let relative = relative.to_string_lossy().replace('\\', "/");
        let bytes = fs::read(source)?;
        digest.update((relative.len() as u64).to_le_bytes());
        digest.update(relative.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    Ok((format!("{:x}", digest.finalize()), files.len()))
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    if is_link_like(source)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing linked input: {}", source.display()),
        ));
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if is_link_like(&from)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing linked input: {}", from.display()),
            ));
        }
        if entry.file_type()?.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(from, to)?;
        }
    }
    Ok(())
}

fn tree_files(root: &Path) -> io::Result<Vec<(PathBuf, PathBuf)>> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> io::Result<()> {
        let mut entries: Vec<_> = fs::read_dir(current)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if is_link_like(&path)? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("refusing linked input: {}", path.display()),
                ));
            }
            if entry.file_type()?.is_dir() {
                visit(root, &path, files)?;
            } else {
                files.push((path.strip_prefix(root).unwrap().to_path_buf(), path));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn is_link_like(path: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Ok(metadata.file_attributes() & 0x400 != 0)
    }
    #[cfg(not(windows))]
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_captures_task_and_oracle_before_acceptance() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init"]);
        git(&repository, &["config", "user.email", "test@example.com"]);
        git(&repository, &["config", "user.name", "Test"]);
        fs::write(repository.join("README.md"), "test\n").unwrap();
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "base"]);
        let task = temp.path().join("task.txt");
        fs::write(&task, "Make the change").unwrap();
        let overlay = temp.path().join("overlay");
        fs::create_dir(&overlay).unwrap();
        fs::write(overlay.join("acceptance.rs"), "test").unwrap();
        let spec = submit(SubmitOptions {
            repository: repository.clone(),
            base_revision: "HEAD".to_string(),
            task_file: task,
            harnesses: vec![HarnessRequest {
                id: "pi".to_string(),
                binary: None,
            }],
            run_order: vec!["pi".to_string()],
            model: ModelPins {
                provider: "openrouter".to_string(),
                model: "deepseek/test".to_string(),
                base_url: "https://example.invalid".to_string(),
                credential_alias: "openrouter-default".to_string(),
                thinking: "high".to_string(),
                max_turns: 10,
                sampling: SAMPLING_OMITTED.to_string(),
            },
            limits: ResourceLimits::default(),
            verifier: VerifierProfile::WorkspaceTests,
            task_aware_context: false,
            acceptance_overlay: Some(overlay),
            experiment: None,
        })
        .unwrap();
        let root = JobStore::for_repository(&repository).job_root(&spec.job_id);
        assert_eq!(
            fs::read_to_string(root.join("input/task.txt")).unwrap(),
            "Make the change"
        );
        assert!(
            root.join("input/acceptance-overlay/acceptance.rs")
                .is_file()
        );
        assert!(root.join("ready").is_file());

        let store = JobStore::for_repository(&repository);
        let job_id = spec.job_id.clone();
        let updater = store.clone();
        let update_id = job_id.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let mut state = updater.load_state(&update_id).unwrap();
            state.revision += 1;
            state.status = JobStatus::Succeeded;
            state.phase = "complete".to_string();
            updater.write_state(&state).unwrap();
        });
        let state =
            await_terminal(&repository, &job_id, Some(Duration::from_secs(2)), None).unwrap();
        assert_eq!(state.status, JobStatus::Succeeded);
        verify_captured_inputs(&spec, &root).unwrap();
        fs::write(root.join("input/task.txt"), "tampered").unwrap();
        assert!(
            verify_captured_inputs(&spec, &root)
                .unwrap_err()
                .to_string()
                .contains("captured task")
        );
    }

    #[test]
    fn build_spec_performs_no_queue_side_effects() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init"]);
        git(&repository, &["config", "user.email", "test@example.com"]);
        git(&repository, &["config", "user.name", "Test"]);
        fs::write(repository.join("README.md"), "test\n").unwrap();
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "base"]);
        let task = temp.path().join("task.txt");
        fs::write(&task, "Make the change").unwrap();

        let spec = build_spec(SubmitOptions {
            repository: repository.clone(),
            base_revision: "HEAD".to_string(),
            task_file: task,
            harnesses: vec![HarnessRequest {
                id: "pi".to_string(),
                binary: None,
            }],
            run_order: vec!["pi".to_string()],
            model: ModelPins {
                provider: "openrouter".to_string(),
                model: "deepseek/test".to_string(),
                base_url: "https://example.invalid".to_string(),
                credential_alias: "openrouter-default".to_string(),
                thinking: "high".to_string(),
                max_turns: 10,
                sampling: SAMPLING_OMITTED.to_string(),
            },
            limits: ResourceLimits::default(),
            verifier: VerifierProfile::WorkspaceTests,
            task_aware_context: false,
            acceptance_overlay: None,
            experiment: None,
        })
        .unwrap();
        assert_eq!(spec.base_revision.len(), 40);
        assert!(
            !JobStore::for_repository(&repository)
                .job_root(&spec.job_id)
                .exists()
        );
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

    #[test]
    fn progress_mtime_tracks_events_and_harness_logs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        assert_eq!(progress_mtime(root).unwrap(), None);
        fs::write(root.join("events.jsonl"), "x\n").unwrap();
        assert!(progress_mtime(root).unwrap().is_some());
        let stdout = root.join("artifacts/harnesses/liberado/session.stdout.log");
        fs::create_dir_all(stdout.parent().unwrap()).unwrap();
        fs::write(&stdout, "y\n").unwrap();
        assert!(progress_mtime(root).unwrap().is_some());
    }
}
