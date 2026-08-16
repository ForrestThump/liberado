//! Crash-safe, append-only job storage.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use ulid::Ulid;

use crate::contract::{ComparisonReport, JobEvent, JobId, JobSpec, JobState};

pub const JOBS_DIRECTORY: &str = ".liberado/harness-jobs";

#[derive(Debug, Clone)]
pub struct JobStore {
    root: PathBuf,
}

impl JobStore {
    pub fn for_repository(repository: &Path) -> Self {
        Self::new(repository.join(JOBS_DIRECTORY))
    }

    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn job_root(&self, job_id: &JobId) -> PathBuf {
        self.root.join(&job_id.0)
    }

    pub fn create(&self, spec: &JobSpec) -> io::Result<PathBuf> {
        self.create_with_inputs(spec, |_| Ok(()))
    }

    pub fn create_with_inputs(
        &self,
        spec: &JobSpec,
        populate: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<PathBuf> {
        spec.validate().map_err(invalid_data)?;
        fs::create_dir_all(&self.root)?;
        let job_root = self.job_root(&spec.job_id);
        fs::create_dir(&job_root).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("job already exists: {}", spec.job_id),
                )
            } else {
                error
            }
        })?;
        fs::create_dir(job_root.join("input"))?;
        fs::create_dir(job_root.join("artifacts"))?;
        if let Err(error) = populate(&job_root) {
            let _ = fs::remove_dir_all(&job_root);
            return Err(error);
        }
        atomic_json(&job_root.join("job.json"), spec)?;
        let state = JobState::accepted(spec.job_id.clone());
        self.write_state(&state)?;
        self.append_job_event(
            &spec.job_id,
            &JobEvent {
                sequence: 0,
                at: state.updated_at,
                status: state.status,
                phase: state.phase.clone(),
                message: "job accepted".to_string(),
            },
        )?;
        atomic_write(&job_root.join("ready"), b"ready\n")?;
        Ok(job_root)
    }

    pub fn load_spec(&self, job_id: &JobId) -> io::Result<JobSpec> {
        let spec: JobSpec = read_json(self.job_root(job_id).join("job.json"))?;
        spec.validate().map_err(invalid_data)?;
        Ok(spec)
    }

    pub fn write_state(&self, state: &JobState) -> io::Result<()> {
        let path = self
            .job_root(&state.job_id)
            .join(format!("state-{:020}.json", state.revision));
        atomic_json(&path, state)
    }

    pub fn load_state(&self, job_id: &JobId) -> io::Result<JobState> {
        let root = self.job_root(job_id);
        let mut candidates = Vec::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("state-") && name.ends_with(".json") {
                candidates.push(entry.path());
            }
        }
        candidates.sort();
        let path = candidates.last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("job has no state records: {job_id}"),
            )
        })?;
        read_json(path)
    }

    /// Append and flush one complete event before execution continues.
    pub fn append_job_event(&self, job_id: &JobId, event: &JobEvent) -> io::Result<()> {
        let path = self.job_root(job_id).join("events.jsonl");
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        serde_json::to_writer(&mut file, event).map_err(invalid_data)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_data()
    }

    pub fn events(&self, job_id: &JobId) -> io::Result<Vec<JobEvent>> {
        let path = self.job_root(job_id).join("events.jsonl");
        let file = File::open(path)?;
        BufReader::new(file)
            .lines()
            .enumerate()
            .map(|(index, line)| {
                let line = line?;
                serde_json::from_str(&line).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid event JSON on line {}: {error}", index + 1),
                    )
                })
            })
            .collect()
    }

    pub fn write_report(&self, report: &ComparisonReport) -> io::Result<()> {
        atomic_json(&self.job_root(&report.job_id).join("report.json"), report)?;
        atomic_write(
            &self.job_root(&report.job_id).join("report.md"),
            render_report(report).as_bytes(),
        )
    }

    pub fn load_report(&self, job_id: &JobId) -> io::Result<ComparisonReport> {
        read_json(self.job_root(job_id).join("report.json"))
    }

    pub fn request_cancel(&self, job_id: &JobId) -> io::Result<()> {
        let path = self.job_root(job_id).join("cancel-requested");
        OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?
            .sync_all()
    }

    pub fn cancellation_requested(&self, job_id: &JobId) -> bool {
        self.job_root(job_id).join("cancel-requested").is_file()
    }

    pub fn acquire_lease(&self, job_id: &JobId) -> io::Result<JobLease> {
        let path = self.job_root(job_id).join("worker.lease");
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let pid = lease_pid(&path).unwrap_or(0);
                if pid != 0 && process_is_alive(pid) {
                    return Err(error);
                }
                fs::remove_file(&path)?;
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?
            }
            Err(error) => return Err(error),
        };
        writeln!(file, "pid={}", std::process::id())?;
        writeln!(file, "started={}", chrono::Utc::now().to_rfc3339())?;
        file.sync_all()?;
        Ok(JobLease { path })
    }

    pub fn accepted_jobs(&self) -> io::Result<Vec<JobId>> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let mut jobs = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let value = entry.file_name().to_string_lossy().into_owned();
            let Ok(job_id) = JobId::parse(&value) else {
                continue;
            };
            if self
                .load_state(&job_id)
                .is_ok_and(|state| !state.status.is_terminal())
                && self.job_root(&job_id).join("ready").is_file()
            {
                jobs.push(job_id);
            }
        }
        jobs.sort();
        Ok(jobs)
    }
}

fn lease_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("pid="))?
        .parse()
        .ok()
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
    liberado_common::process::std_command("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

/// Removing the lease is safe here: only the owner receives this value.
pub struct JobLease {
    path: PathBuf,
}

impl Drop for JobLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn atomic_json(path: &Path, value: &impl serde::Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(invalid_data)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "atomic target has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        Ulid::new()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> io::Result<T> {
    serde_json::from_slice(&fs::read(path)?).map_err(invalid_data)
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn render_report(report: &ComparisonReport) -> String {
    let mut text = format!(
        "# Harness comparison {}\n\nStatus: `{:?}`\n\nExperiment: `{}`\n\n",
        report.job_id, report.status, report.experiment_id
    );
    for result in report.harnesses.values() {
        text.push_str(&format!(
            "- {}: accepted={}, harness exit={:?}, verifier exit={:?}, head={}\n",
            result.harness,
            result.accepted,
            result.exit_code,
            result.verifier_exit_code,
            result.head_commit.as_deref().unwrap_or("none")
        ));
        let duration = result
            .duration_secs
            .map(|secs| format!("{secs:.1}s"))
            .unwrap_or_else(|| "unknown".to_string());
        let turns = result
            .turns_used
            .map(|turns| turns.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let tokens = match (result.tokens_in, result.tokens_out) {
            (Some(input), Some(output)) => format!("{input} in / {output} out"),
            _ => "unknown".to_string(),
        };
        let window = match (result.started_at, result.finished_at) {
            (Some(start), Some(finish)) => {
                format!(" ({} -> {})", start.to_rfc3339(), finish.to_rfc3339())
            }
            _ => String::new(),
        };
        text.push_str(&format!(
            "  - wall-clock: {}{}\n  - turns: {}\n  - tokens: {}\n",
            duration, window, turns, tokens
        ));
    }
    if !report.diagnostics.is_empty() {
        text.push_str("\nDiagnostics:\n\n");
        for diagnostic in &report.diagnostics {
            text.push_str(&format!("- {diagnostic}\n"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::*;
    use chrono::Utc;

    fn spec() -> JobSpec {
        JobSpec {
            version: JOB_SPEC_VERSION,
            job_id: JobId::new(),
            submitted_at: Utc::now(),
            repository: PathBuf::from("C:/repo"),
            base_revision: "main".to_string(),
            task: TaskBundle::new("task.txt", "test task".to_string()).unwrap(),
            harnesses: vec![HarnessRequest {
                id: "liberado".to_string(),
                binary: None,
            }],
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
            acceptance: None,
            experiment: None,
            experiment_id: String::new(),
        }
        .finalize()
        .unwrap()
    }

    #[test]
    fn state_records_are_append_only_and_latest_wins() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path().join("jobs"));
        let spec = spec();
        store.create(&spec).unwrap();
        let mut state = store.load_state(&spec.job_id).unwrap();
        state.revision = 1;
        state.status = JobStatus::Running;
        state.phase = "running".to_string();
        store.write_state(&state).unwrap();
        assert_eq!(
            store.load_state(&spec.job_id).unwrap().status,
            JobStatus::Running
        );
        assert!(
            store
                .job_root(&spec.job_id)
                .join("state-00000000000000000000.json")
                .is_file()
        );
    }

    #[test]
    fn malformed_event_is_reported_instead_of_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path().join("jobs"));
        let spec = spec();
        store.create(&spec).unwrap();
        let valid = serde_json::to_string(&JobEvent {
            sequence: 0,
            at: Utc::now(),
            status: JobStatus::Accepted,
            phase: "accepted".to_string(),
            message: "ok".to_string(),
        })
        .unwrap();
        fs::write(
            store.job_root(&spec.job_id).join("events.jsonl"),
            format!("{valid}\nnot-json\n"),
        )
        .unwrap();
        let error = store.events(&spec.job_id).unwrap_err();
        assert!(error.to_string().contains("line 2"));
    }
}
