//! Generic **preflight** gate — ordered shell steps, fail-closed, language-agnostic.
//!
//! Nothing is ready or shippable until project preflight passes. This module is the runner only;
//! project config supplies steps (not hard-coded cargo). Do **not** re-execute GitHub Actions YAML
//! here — share the same commands or scripts CI uses.
//!
//! Steps always run to completion of the profile (no fail-fast on the first required failure). A
//! required failure sets overall `ok` false but later steps still execute so the report carries the
//! complete actionable failure set (format, compile, test, and clippy can all show up together).

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

/// Liberado project **ship** steps for the agent host bar.
///
/// Staging (0.1b): **format** first (action), then **compile**, then **test** and **clippy** as
/// co-staged diagnostics, then **deny**. The runner does not stop after the first required
/// failure, so the model sees the full failure set in one report.
///
/// Not multi-OS; remote CI still owns the matrix. Prefer project config / shared script when
/// present; this is the built-in default for project `liberado` when config omits steps.
///
/// `--no-fail-fast` on the test step for the same reason CI uses it: cargo otherwise stops at the
/// first failing test binary, so the failures it reports are truncated. A truncated set breaks
/// [`diff_against_baseline`] — fixing an early failure lets cargo reach later ones, and they then
/// look like new regressions caused by the fix.
pub fn liberado_ship_preflight_steps() -> Vec<PreflightStep> {
    vec![
        PreflightStep::new("fmt", "cargo fmt --check"),
        PreflightStep::new("compile", "cargo check --workspace --all-targets"),
        PreflightStep::new("test", "cargo test --workspace --no-fail-fast"),
        PreflightStep::new(
            "clippy",
            "cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings",
        ),
        PreflightStep::new("deny", "cargo deny check"),
    ]
}

/// What failed in one step, as a set of stable identities.
///
/// Keyed by step name so the same test name failing under two different steps stays two facts.
pub type FailureSet = std::collections::BTreeMap<String, std::collections::BTreeSet<String>>;

/// Pull stable identities out of one step's log.
///
/// Identity, never count: a count can stay flat while one test starts failing and another stops,
/// which is a regression a numeric check waves through.
///
/// Recognised today:
/// * `test <name> ... FAILED` — cargo test, the dominant case.
/// * `RUSTSEC-YYYY-NNNN` — cargo-deny advisories. These appear on their own as the world
///   publishes CVEs, with no change to the code, so forgiving pre-existing ones matters as much
///   here as for tests.
///
/// Anything else that failed collapses to the opaque marker [`OPAQUE_FAILURE`]. That is
/// deliberately coarse: for a step like `fmt` or `clippy`, a base that was already failing
/// forgives *any* failure of that step. It is a bounded, documented hole, and still strictly
/// better than the alternative — a gate the agent cannot pass and cannot fix.
pub fn failure_identities(log: &str) -> std::collections::BTreeSet<String> {
    let mut found = std::collections::BTreeSet::new();
    for line in log.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("test ")
            && let Some(name) = rest.split(" ... ").next()
            && rest.contains("... FAILED")
            && !name.is_empty()
            && !name.starts_with("result:")
        {
            found.insert(name.trim().to_string());
        }
        if let Some(idx) = line.find("RUSTSEC-") {
            let id: String = line[idx..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if id.len() > "RUSTSEC-".len() {
                found.insert(id);
            }
        }
    }
    found
}

/// Marker for a failing step whose log yielded no parseable identity.
pub const OPAQUE_FAILURE: &str = "<step failed>";

/// Identities for every failing step in a report.
pub fn report_failures(report: &PreflightReport) -> FailureSet {
    let mut out = FailureSet::new();
    for step in &report.steps {
        if step.ok {
            continue;
        }
        let mut ids = failure_identities(&step.log_excerpt);
        if ids.is_empty() {
            ids.insert(OPAQUE_FAILURE.to_string());
        }
        out.insert(step.name.clone(), ids);
    }
    out
}

/// Failures present now but not in `baseline`, per step.
///
/// This is the whole point of the gate: "did you break something" and "was it already broken"
/// need opposite responses, and an absolute pass/fail cannot tell them apart. Requiring absolute
/// green locks out the entire class of work that *fixes* a red base.
pub fn diff_against_baseline(current: &FailureSet, baseline: &FailureSet) -> FailureSet {
    let mut new = FailureSet::new();
    for (step, ids) in current {
        let known = baseline.get(step);
        let fresh: std::collections::BTreeSet<String> = ids
            .iter()
            .filter(|id| known.is_none_or(|k| !k.contains(*id)))
            .cloned()
            .collect();
        if !fresh.is_empty() {
            new.insert(step.clone(), fresh);
        }
    }
    new
}

/// Flatten a [`FailureSet`] to `step: identity` lines, for prompts and logs.
pub fn describe_failures(set: &FailureSet) -> Vec<String> {
    set.iter()
        .flat_map(|(step, ids)| ids.iter().map(move |id| format!("{step}: {id}")))
        .collect()
}

pub fn liberado_ship_preflight_spec() -> PreflightSpec {
    PreflightSpec::new("ship", liberado_ship_preflight_steps())
}

/// Run every step in `spec` under `workspace_root` (staged reporting: no fail-fast).
///
/// A failing **required** step sets overall `ok` to false but does **not** stop later steps, so
/// the report can carry format, compile, test, and clippy failures together. Optional failures
/// never fail the profile. Ship success stays fail-closed when any required step failed.
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
        // Continue after failures so the report carries the full failure set (0.1b).
        if !ok && step.required {
            overall_ok = false;
        }
    }

    // Recompute from required steps only (guards optional-only failures).
    for r in &results {
        if !r.ok {
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
        let failed: Vec<&str> = results
            .iter()
            .filter(|r| !r.ok)
            .map(|r| r.name.as_str())
            .collect();
        format!(
            "preflight '{}': failed at step(s) '{}' ({} step result(s), {}ms)",
            spec.id,
            failed.join(", "),
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
        let mut c = liberado_common::process::command("cmd");
        c.args(["/C", command_line]);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = liberado_common::process::command("sh");
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
    if let Some(steps) = configured_steps
        && !steps.is_empty()
    {
        return Some(PreflightSpec::new("ship", steps));
    }
    if project_name.is_some_and(|n| n.eq_ignore_ascii_case("liberado")) {
        return Some(liberado_ship_preflight_spec());
    }
    None
}

#[cfg(test)]
#[path = "preflight_differential_tests.rs"]
mod differential_tests;

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "preflight_survivor_tests.rs"]
mod survivor_tests;
