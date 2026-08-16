//! Versioned wire and disk contracts for harness-comparison jobs.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

pub const JOB_SPEC_VERSION: u32 = 1;
pub const WORKER_CONFIG_VERSION: u32 = 1;

/// The only sampling value the v1 coordinator records today: no temperature is passed to either
/// client, so "omitted" is the honest pin. Kept as a named constant so every pin, default, and
/// validation agrees on the spelling. A later change that actually applies a temperature to both
/// clients replaces this with a real value.
pub const SAMPLING_OMITTED: &str = "omitted";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub String);

impl JobId {
    pub fn new() -> Self {
        Self(Ulid::new().to_string().to_ascii_lowercase())
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        if value.len() != 26
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return Err("job id must be a lowercase ULID".to_string());
        }
        Ulid::from_string(&value.to_ascii_uppercase())
            .map_err(|_| "job id is not a valid ULID".to_string())?;
        Ok(Self(value.to_string()))
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBundle {
    pub source_name: String,
    pub text: String,
    pub sha256: String,
}

impl TaskBundle {
    pub fn new(source_name: impl Into<String>, text: String) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Err("comparison task is empty".to_string());
        }
        let sha256 = sha256(text.as_bytes());
        Ok(Self {
            source_name: source_name.into(),
            text,
            sha256,
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.text.trim().is_empty() {
            return Err("comparison task is empty".to_string());
        }
        if sha256(self.text.as_bytes()) != self.sha256 {
            return Err("comparison task digest does not match its content".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceBundle {
    /// Job-relative directory populated before the job is accepted.
    pub directory: PathBuf,
    pub sha256: String,
    pub file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessRequest {
    pub id: String,
    #[serde(default)]
    pub binary: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPins {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub credential_alias: String,
    pub thinking: String,
    pub max_turns: u32,
    /// Declared sampling policy for both clients.
    ///
    /// The v1 coordinator does not yet pass a temperature to either client, so [`SAMPLING_OMITTED`]
    /// is the only honest value today. Recording it here makes the decision an immutable
    /// experiment pin (it is part of the experiment id) and shows it in `experiment.json`.
    #[serde(default = "default_sampling")]
    pub sampling: String,
}

fn default_sampling() -> String {
    SAMPLING_OMITTED.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub compile_timeout_secs: u64,
    pub run_timeout_secs: u64,
    pub minimum_free_bytes: u64,
    /// Additional model sessions allowed after a common verifier failure.
    ///
    /// This is deliberately zero by default. Benchmark comparisons must measure the harness's
    /// native first-pass behavior; assisted repair is an explicit production policy.
    #[serde(default = "default_verifier_repair_attempts")]
    pub verifier_repair_attempts: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            compile_timeout_secs: 3_600,
            run_timeout_secs: 14_400,
            minimum_free_bytes: 20 * 1024 * 1024 * 1024,
            verifier_repair_attempts: default_verifier_repair_attempts(),
        }
    }
}

fn default_verifier_repair_attempts() -> u32 {
    0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierProfile {
    WorkspaceTests,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Experiment {
    pub hypothesis: String,
    pub variable: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub version: u32,
    pub job_id: JobId,
    pub submitted_at: DateTime<Utc>,
    pub repository: PathBuf,
    pub base_revision: String,
    pub task: TaskBundle,
    pub harnesses: Vec<HarnessRequest>,
    pub model: ModelPins,
    pub limits: ResourceLimits,
    pub verifier: VerifierProfile,
    #[serde(default)]
    pub task_aware_context: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<AcceptanceBundle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment: Option<Experiment>,
    pub experiment_id: String,
}

impl JobSpec {
    pub fn finalize(mut self) -> Result<Self, String> {
        self.validate_without_experiment_id()?;
        self.experiment_id = self.compute_experiment_id()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_without_experiment_id()?;
        if self.compute_experiment_id()? != self.experiment_id {
            return Err("experiment id does not match the immutable job pins".to_string());
        }
        Ok(())
    }

    fn validate_without_experiment_id(&self) -> Result<(), String> {
        if self.version != JOB_SPEC_VERSION {
            return Err(format!("unsupported job spec version {}", self.version));
        }
        JobId::parse(&self.job_id.0)?;
        self.task.validate()?;
        if self.repository.as_os_str().is_empty() {
            return Err("repository path is empty".to_string());
        }
        if self.base_revision.trim().is_empty() {
            return Err("base revision is empty".to_string());
        }
        if self.harnesses.is_empty() {
            return Err("at least one harness is required".to_string());
        }
        let mut ids = std::collections::BTreeSet::new();
        for harness in &self.harnesses {
            if !matches!(harness.id.as_str(), "liberado" | "pi") {
                return Err(format!("unsupported harness '{}'", harness.id));
            }
            if !ids.insert(&harness.id) {
                return Err(format!("duplicate harness '{}'", harness.id));
            }
        }
        if self.model.provider.trim().is_empty()
            || self.model.model.trim().is_empty()
            || self.model.credential_alias.trim().is_empty()
        {
            return Err("provider, model, and credential alias are required".to_string());
        }
        if self.model.max_turns == 0
            || self.limits.compile_timeout_secs == 0
            || self.limits.run_timeout_secs == 0
        {
            return Err("turn and time limits must be positive".to_string());
        }
        if self.model.sampling != SAMPLING_OMITTED {
            return Err(format!(
                "sampling pin '{}' is not yet applied by either client; only '{}' is supported",
                self.model.sampling, SAMPLING_OMITTED
            ));
        }
        if let Some(acceptance) = &self.acceptance {
            if acceptance.directory.is_absolute()
                || acceptance
                    .directory
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err("acceptance directory must stay inside the job".to_string());
            }
            if acceptance.file_count == 0 {
                return Err("acceptance bundle contains no files".to_string());
            }
        }
        Ok(())
    }

    fn compute_experiment_id(&self) -> Result<String, String> {
        #[derive(Serialize)]
        struct Pins<'a> {
            repository: &'a PathBuf,
            base_revision: &'a str,
            task_sha256: &'a str,
            harnesses: &'a [HarnessRequest],
            model: &'a ModelPins,
            limits: &'a ResourceLimits,
            verifier: &'a VerifierProfile,
            task_aware_context: bool,
            acceptance_sha256: Option<&'a str>,
            experiment: &'a Option<Experiment>,
        }
        let bytes = serde_json::to_vec(&Pins {
            repository: &self.repository,
            base_revision: &self.base_revision,
            task_sha256: &self.task.sha256,
            harnesses: &self.harnesses,
            model: &self.model,
            limits: &self.limits,
            verifier: &self.verifier,
            task_aware_context: self.task_aware_context,
            acceptance_sha256: self.acceptance.as_ref().map(|value| value.sha256.as_str()),
            experiment: &self.experiment,
        })
        .map_err(|error| error.to_string())?;
        Ok(sha256(&bytes))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Accepted,
    Preflight,
    Preparing,
    Running,
    Verifying,
    Preserving,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    TaskFailure,
    VerifierFailure,
    HarnessFailure,
    Timeout,
    HostInfrastructureFailure,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobState {
    pub job_id: JobId,
    pub status: JobStatus,
    pub phase: String,
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<FailureClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl JobState {
    pub fn accepted(job_id: JobId) -> Self {
        Self {
            job_id,
            status: JobStatus::Accepted,
            phase: "accepted".to_string(),
            revision: 0,
            updated_at: Utc::now(),
            failure_class: None,
            message: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEvent {
    pub sequence: u64,
    pub at: DateTime<Utc>,
    pub status: JobStatus,
    pub phase: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessResult {
    pub harness: String,
    pub exit_code: Option<i32>,
    pub verifier_exit_code: Option<i32>,
    pub head_commit: Option<String>,
    pub archive_branch: Option<String>,
    pub accepted: bool,
    #[serde(default)]
    pub diagnostics: Vec<String>,
    /// Wall-clock start of the harness run, parsed from its `run-status.txt`. `None` when the
    /// adapter produced no run-status (e.g. a launch failure before the process spawned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// Wall-clock end of the harness run, parsed from its `run-status.txt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// Run duration in seconds, derived from `started_at`/`finished_at`. Kept as a first-class field
    /// so scoreboards never have to recompute it by hand (the source of historical analysis errors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    /// Model turns consumed, parsed from the harness's native transcript (Liberado `coder-traces`,
    /// pi `session.jsonl`). `None` when the transcript is absent or unparseable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns_used: Option<u32>,
    /// Prompt tokens consumed across the run, parsed from the harness's native transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_in: Option<u64>,
    /// Completion tokens produced across the run, parsed from the harness's native transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_out: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonReport {
    pub version: u32,
    pub job_id: JobId,
    pub experiment_id: String,
    pub status: JobStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<FailureClass>,
    pub base_commit: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub harnesses: BTreeMap<String, HarnessResult>,
    #[serde(default)]
    pub diagnostics: Vec<String>,
    pub artifact_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerPolicy {
    pub version: u32,
    pub repositories: Vec<PathBuf>,
    pub providers: Vec<String>,
    #[serde(default = "default_base_urls")]
    pub base_urls: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub model_prefixes: Vec<String>,
    pub maximum_turns: u32,
    pub maximum_compile_timeout_secs: u64,
    pub maximum_run_timeout_secs: u64,
    pub minimum_free_bytes: u64,
    #[serde(default = "default_estimated_build_bytes")]
    pub estimated_build_bytes_per_harness: u64,
    #[serde(default)]
    pub retain_build_caches: bool,
    #[serde(default)]
    pub retain_worktrees: bool,
    #[serde(default)]
    pub allow_binary_overrides: bool,
    pub poll_interval_ms: u64,
    pub credential_aliases: BTreeMap<String, String>,
}

impl WorkerPolicy {
    pub fn for_repository(repository: PathBuf) -> Self {
        Self {
            version: WORKER_CONFIG_VERSION,
            repositories: vec![repository],
            providers: vec!["openrouter".to_string()],
            base_urls: default_base_urls(),
            model_prefixes: vec!["deepseek/".to_string()],
            maximum_turns: 400,
            maximum_compile_timeout_secs: 3_600,
            maximum_run_timeout_secs: 14_400,
            minimum_free_bytes: 20 * 1024 * 1024 * 1024,
            estimated_build_bytes_per_harness: default_estimated_build_bytes(),
            retain_build_caches: false,
            retain_worktrees: false,
            allow_binary_overrides: false,
            poll_interval_ms: 30_000,
            credential_aliases: BTreeMap::from([(
                "openrouter-default".to_string(),
                "OPENROUTER_API_KEY".to_string(),
            )]),
        }
    }
}

fn default_base_urls() -> BTreeMap<String, Vec<String>> {
    BTreeMap::from([(
        "openrouter".to_string(),
        vec!["https://openrouter.ai/api/v1".to_string()],
    )])
}

fn default_estimated_build_bytes() -> u64 {
    15 * 1024 * 1024 * 1024
}

pub fn sha256(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> JobSpec {
        JobSpec {
            version: JOB_SPEC_VERSION,
            job_id: JobId::new(),
            submitted_at: Utc::now(),
            repository: PathBuf::from("C:/repo"),
            base_revision: "main".to_string(),
            task: TaskBundle::new("task.txt", "Fix the item".to_string()).unwrap(),
            harnesses: vec![
                HarnessRequest {
                    id: "liberado".to_string(),
                    binary: None,
                },
                HarnessRequest {
                    id: "pi".to_string(),
                    binary: None,
                },
            ],
            model: ModelPins {
                provider: "openrouter".to_string(),
                model: "deepseek/test".to_string(),
                base_url: "https://openrouter.ai/api/v1".to_string(),
                credential_alias: "openrouter-default".to_string(),
                thinking: "high".to_string(),
                max_turns: 400,
                sampling: SAMPLING_OMITTED.to_string(),
            },
            limits: ResourceLimits::default(),
            verifier: VerifierProfile::WorkspaceTests,
            task_aware_context: true,
            acceptance: None,
            experiment: None,
            experiment_id: String::new(),
        }
        .finalize()
        .unwrap()
    }

    #[test]
    fn immutable_pin_change_invalidates_experiment_id() {
        let mut value = spec();
        value.model.max_turns -= 1;
        assert!(value.validate().unwrap_err().contains("experiment id"));
    }

    #[test]
    fn task_content_is_bound_to_its_digest() {
        let mut value = spec();
        value.task.text.push_str(" changed");
        assert!(value.validate().unwrap_err().contains("task digest"));
    }

    #[test]
    fn verifier_repairs_are_opt_in_for_fair_comparisons() {
        assert_eq!(ResourceLimits::default().verifier_repair_attempts, 0);
    }

    #[test]
    fn sampling_pin_rejects_values_not_applied_by_either_client() {
        let mut value = spec();
        value.model.sampling = "0.1".to_string();
        assert!(value.validate().unwrap_err().contains("sampling"));
    }
}
