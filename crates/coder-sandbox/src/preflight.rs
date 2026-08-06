//! Generic **preflight** gate — ordered shell steps, fail-closed, language-agnostic.
//!
//! Product idea (see `docs/future-work/self-pr-quality-roadmap.md`): nothing is ready/shippable
//! until project preflight passes. This module is the runner only; project config supplies steps
//! (not hard-coded cargo). Do **not** re-execute GitHub Actions YAML here — share the same
//! commands/scripts CI uses.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::Command;
use tokio::time::timeout;

use crate::{path_for_cli, strip_extended_path_prefix};

/// Default per-step timeout when the step does not set one (full workspace test can be long).
pub const DEFAULT_STEP_TIMEOUT_SECS: u64 = 45 * 60;

/// Cap log excerpt stored on each step result (full output is truncated).
pub const DEFAULT_LOG_CAP_BYTES: usize = 16 * 1024;

/// One named command in a preflight profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightStep {
    pub name: String,
    /// Full shell command line (e.g. `cargo test --workspace`). Run via `sh -c` / `cmd /C`.
    pub run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// When false, a non-zero exit is recorded but does not fail the profile.
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

impl PreflightStep {
    pub fn new(name: impl Into<String>, run: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            run: run.into(),
            timeout_secs: None,
            required: true,
        }
    }

    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }
}

/// Named profile of ordered steps (e.g. `ship`, `fast`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PreflightSpec {
    /// Profile id (`ship`, `fast`, …).
    pub id: String,
    #[serde(default)]
    pub steps: Vec<PreflightStep>,
}

impl PreflightSpec {
    pub fn new(id: impl Into<String>, steps: Vec<PreflightStep>) -> Self {
        Self {
            id: id.into(),
            steps,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Outcome of one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightStepResult {
    pub name: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub timed_out: bool,
    pub ok: bool,
    /// Truncated combined stdout+stderr.
    pub log_excerpt: String,
}

/// Full preflight run report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightReport {
    pub profile_id: String,
    pub ok: bool,
    pub steps: Vec<PreflightStepResult>,
    pub summary: String,
    pub duration_ms: u64,
}

#[derive(Debug, Error)]
pub enum PreflightError {
    #[error("preflight workspace root missing: {0}")]
    MissingRoot(String),
    #[error("preflight profile has no steps")]
    EmptySpec,
    #[error("preflight spawn failed: {0}")]
    Spawn(String),
}

/// Liberado project **ship** steps matching `.github/workflows/ci.yml` on the agent host
/// (fmt, clippy with the same exclude/`-D warnings`, workspace test, cargo-deny).
///
/// Not multi-OS; remote CI still owns the matrix. Prefer project config / shared script when
/// present; this is the built-in default for project `liberado` when config omits steps.
pub fn liberado_ship_preflight_steps() -> Vec<PreflightStep> {
    vec![
        PreflightStep::new("fmt", "cargo fmt --check"),
        PreflightStep::new(
            "clippy",
            "cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings",
        ),
        PreflightStep::new("test", "cargo test --workspace"),
        PreflightStep::new("deny", "cargo deny check"),
    ]
}

pub fn liberado_ship_preflight_spec() -> PreflightSpec {
    PreflightSpec::new("ship", liberado_ship_preflight_steps())
}

/// Run `spec` fail-fast (stop after first failing **required** step) under `workspace_root`.
pub async fn run_preflight(
    workspace_root: &Path,
    spec: &PreflightSpec,
) -> Result<PreflightReport, PreflightError> {
    run_preflight_with_options(workspace_root, spec, DEFAULT_LOG_CAP_BYTES).await
}

pub async fn run_preflight_with_options(
    workspace_root: &Path,
    spec: &PreflightSpec,
    log_cap_bytes: usize,
) -> Result<PreflightReport, PreflightError> {
    if spec.steps.is_empty() {
        return Err(PreflightError::EmptySpec);
    }
    let root = workspace_root
        .canonicalize()
        .map_err(|_| PreflightError::MissingRoot(workspace_root.display().to_string()))?;
    let root = strip_extended_path_prefix(&root);
    let root_cli = path_for_cli(&root);

    let started = Instant::now();
    let mut results = Vec::with_capacity(spec.steps.len());
    let mut overall_ok = true;

    for step in &spec.steps {
        let step_start = Instant::now();
        let timeout_secs = step
            .timeout_secs
            .unwrap_or(DEFAULT_STEP_TIMEOUT_SECS)
            .max(1);
        let outcome = run_step_shell(&root_cli, &step.run, timeout_secs).await?;
        let duration_ms = step_start.elapsed().as_millis() as u64;
        let ok = !outcome.timed_out && outcome.exit_code == Some(0);
        let log_excerpt = cap_log(&outcome.combined, log_cap_bytes);
        results.push(PreflightStepResult {
            name: step.name.clone(),
            exit_code: outcome.exit_code,
            duration_ms,
            timed_out: outcome.timed_out,
            ok,
            log_excerpt,
        });
        if !ok && step.required {
            overall_ok = false;
            break;
        }
        if !ok {
            // optional step failed — continue
            overall_ok = overall_ok && !step.required;
        }
    }

    // If we only ran optional failures, overall_ok may still be true.
    for r in &results {
        if !r.ok {
            // recompute from required steps only — use original step flags
            let required = spec
                .steps
                .iter()
                .find(|s| s.name == r.name)
                .map(|s| s.required)
                .unwrap_or(true);
            if required {
                overall_ok = false;
            }
        }
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    let summary = if overall_ok {
        format!(
            "preflight '{}': ok ({} step(s), {}ms)",
            spec.id,
            results.len(),
            duration_ms
        )
    } else {
        let failed = results
            .iter()
            .find(|r| !r.ok)
            .map(|r| r.name.as_str())
            .unwrap_or("?");
        format!(
            "preflight '{}': failed at step '{failed}' ({} step result(s), {}ms)",
            spec.id,
            results.len(),
            duration_ms
        )
    };

    Ok(PreflightReport {
        profile_id: spec.id.clone(),
        ok: overall_ok,
        steps: results,
        summary,
        duration_ms,
    })
}

struct StepOutcome {
    exit_code: Option<i32>,
    timed_out: bool,
    combined: String,
}

async fn run_step_shell(
    cwd: &str,
    command_line: &str,
    timeout_secs: u64,
) -> Result<StepOutcome, PreflightError> {
    let mut cmd = shell_command(command_line);
    cmd.current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let fut = cmd.output();
    match timeout(Duration::from_secs(timeout_secs), fut).await {
        Ok(Ok(output)) => {
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            Ok(StepOutcome {
                exit_code: output.status.code(),
                timed_out: false,
                combined,
            })
        }
        Ok(Err(e)) => Err(PreflightError::Spawn(e.to_string())),
        Err(_) => Ok(StepOutcome {
            exit_code: None,
            timed_out: true,
            combined: format!("timed out after {timeout_secs}s"),
        }),
    }
}

fn shell_command(command_line: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.args(["/C", command_line]);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", command_line]);
        c
    }
}

fn cap_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let start = s.len().saturating_sub(max);
    // Prefer char boundary
    let mut i = start;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    format!("…{}", &s[i..])
}

/// Resolve a ship preflight spec from optional config steps, falling back to liberado defaults
/// when `project_name` is `liberado` and steps are empty.
pub fn resolve_ship_spec(
    project_name: Option<&str>,
    configured_steps: Option<Vec<PreflightStep>>,
) -> Option<PreflightSpec> {
    if let Some(steps) = configured_steps {
        if !steps.is_empty() {
            return Some(PreflightSpec::new("ship", steps));
        }
    }
    if project_name.is_some_and(|n| n.eq_ignore_ascii_case("liberado")) {
        return Some(liberado_ship_preflight_spec());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn all_green_report_when_every_step_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let spec = PreflightSpec::new(
            "ship",
            vec![
                PreflightStep::new("one", "echo ok1"),
                PreflightStep::new("two", "echo ok2"),
            ],
        );
        let report = run_preflight(dir.path(), &spec).await.unwrap();
        assert!(report.ok, "{:?}", report.summary);
        assert_eq!(report.steps.len(), 2);
        assert!(report.steps.iter().all(|s| s.ok));
        assert!(report.summary.contains("ok"));
        assert_eq!(report.profile_id, "ship");
    }

    #[tokio::test]
    async fn fail_fast_on_first_required_failure() {
        let dir = tempfile::tempdir().unwrap();
        let fail = if cfg!(windows) {
            "exit /B 1"
        } else {
            "exit 1"
        };
        let spec = PreflightSpec::new(
            "ship",
            vec![
                PreflightStep::new("pass", "echo hi"),
                PreflightStep::new("boom", fail),
                PreflightStep::new("never", "echo should-not-run"),
            ],
        );
        let report = run_preflight(dir.path(), &spec).await.unwrap();
        assert!(!report.ok);
        assert_eq!(report.steps.len(), 2, "must stop after first required fail");
        assert!(report.steps[0].ok);
        assert!(!report.steps[1].ok);
        assert!(report.summary.contains("boom"));
    }

    #[tokio::test]
    async fn empty_spec_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_preflight(dir.path(), &PreflightSpec::new("ship", vec![]))
            .await
            .unwrap_err();
        assert!(matches!(err, PreflightError::EmptySpec));
    }

    #[tokio::test]
    async fn missing_root_is_error() {
        let err = run_preflight(
            Path::new("/nonexistent/preflight/root-xyz"),
            &PreflightSpec::new("ship", vec![PreflightStep::new("x", "echo x")]),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, PreflightError::MissingRoot(_)));
    }

    #[test]
    fn resolve_ship_spec_uses_configured_steps() {
        let steps = vec![PreflightStep::new("t", "echo t")];
        let spec = resolve_ship_spec(Some("other"), Some(steps.clone())).unwrap();
        assert_eq!(spec.steps, steps);
    }

    #[test]
    fn resolve_ship_spec_liberado_default_when_empty() {
        let spec = resolve_ship_spec(Some("liberado"), None).unwrap();
        assert_eq!(spec.id, "ship");
        let names: Vec<_> = spec.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fmt", "clippy", "test", "deny"]);
        assert!(spec.steps.iter().any(|s| s.run.contains("cargo test --workspace")));
        assert!(spec
            .steps
            .iter()
            .any(|s| s.run.contains("exclude liberado-webui") && s.run.contains("-D warnings")));
    }

    #[test]
    fn resolve_ship_spec_none_for_unknown_project_without_config() {
        assert!(resolve_ship_spec(Some("notes"), None).is_none());
        assert!(resolve_ship_spec(None, None).is_none());
    }

    #[test]
    fn cap_log_exact_boundary() {
        assert_eq!(cap_log("hello", 5), "hello");
        assert_eq!(cap_log("hello", 50), "hello");
    }

    #[test]
    fn cap_log_truncation_keeps_last_max_bytes() {
        let s = "abcdefghijklmnop";
        let capped = cap_log(s, 8);
        assert!(capped.starts_with('…'));
        assert_eq!(capped.len(), 11); // 3-byte … + 8 content bytes
    }

    #[test]
    fn cap_log_empty_string() {
        assert_eq!(cap_log("", 8), "");
    }

    #[test]
    fn cap_log_zero_max_returns_ellipsis_only() {
        assert_eq!(cap_log("anything", 0), "…");
    }

    #[test]
    fn cap_log_multibyte_utf8_truncation_safe() {
        // 字 is 3 bytes; truncation near a multi-byte boundary must not panic.
        let s = "ab字defg";
        let capped = cap_log(s, 5);
        assert!(capped.starts_with('…'));
        // 3-byte … + up to 5 bytes content, may be shorter due to char boundary.
        assert!(capped.len() >= 4 && capped.len() <= 8);
    }

    #[test]
    fn resolve_ship_spec_empty_configured_for_non_liberado_is_none() {
        assert!(resolve_ship_spec(Some("notes"), Some(vec![])).is_none());
    }

    #[test]
    fn resolve_ship_spec_empty_configured_for_liberado_falls_back() {
        let spec = resolve_ship_spec(Some("liberado"), Some(vec![])).unwrap();
        assert!(!spec.steps.is_empty());
        assert_eq!(spec.id, "ship");
    }

    #[test]
    fn preflight_step_builder() {
        let s = PreflightStep::new("lint", "cargo clippy").with_timeout_secs(120);
        assert_eq!(s.name, "lint");
        assert_eq!(s.run, "cargo clippy");
        assert_eq!(s.timeout_secs, Some(120));
        assert!(s.required);
    }

    #[test]
    fn preflight_spec_is_empty() {
        assert!(PreflightSpec::new("x", vec![]).is_empty());
        assert!(!PreflightSpec::new("x", vec![PreflightStep::new("y", "z")]).is_empty());
    }

    #[test]
    fn liberado_ship_preflight_steps_structure() {
        let steps = liberado_ship_preflight_steps();
        let names: Vec<_> = steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fmt", "clippy", "test", "deny"]);
        for s in &steps {
            assert!(s.required, "{} must be required", s.name);
            assert!(!s.run.is_empty(), "{} must have a run command", s.name);
        }
    }

    #[tokio::test]
    async fn optional_failure_does_not_fail_profile() {
        let dir = tempfile::tempdir().unwrap();
        let fail = if cfg!(windows) {
            "exit /B 1"
        } else {
            "exit 1"
        };
        let mut optional = PreflightStep::new("soft", fail);
        optional.required = false;
        let spec = PreflightSpec::new(
            "ship",
            vec![optional, PreflightStep::new("hard", "echo ok")],
        );
        let report = run_preflight(dir.path(), &spec).await.unwrap();
        assert!(report.ok, "{:?}", report);
        assert_eq!(report.steps.len(), 2);
    }
}
