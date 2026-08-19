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
mod differential_tests {
    use super::*;

    fn set(step: &str, ids: &[&str]) -> FailureSet {
        let mut m = FailureSet::new();
        m.insert(
            step.to_string(),
            ids.iter().map(|s| s.to_string()).collect(),
        );
        m
    }

    #[test]
    fn parses_cargo_test_failures_and_ignores_the_summary_line() {
        let log = "\
running 3 tests
test gates::foo ... ok
test gates::bar ... FAILED
test gates::baz ... FAILED
test result: FAILED. 1 passed; 2 failed; 0 ignored
";
        let ids = failure_identities(log);
        assert_eq!(
            ids.iter().cloned().collect::<Vec<_>>(),
            vec!["gates::bar".to_string(), "gates::baz".to_string()],
            "the `test result: FAILED.` summary must not be mistaken for a test name"
        );
    }

    /// Advisories appear with no code change at all, as the world publishes CVEs. Forgiving a
    /// pre-existing one matters as much as forgiving a pre-existing test failure — otherwise
    /// every goal starts failing overnight on something the agent cannot fix.
    #[test]
    fn parses_cargo_deny_advisory_ids() {
        let ids = failure_identities("error[vulnerability]: crate has RUSTSEC-2024-0011 filed");
        assert!(ids.contains("RUSTSEC-2024-0011"), "{ids:?}");
    }

    #[test]
    fn unparseable_failure_collapses_to_the_opaque_marker() {
        let report = PreflightReport {
            profile_id: "ship".into(),
            ok: false,
            duration_ms: 1,
            summary: String::new(),
            steps: vec![PreflightStepResult {
                name: "fmt".into(),
                exit_code: Some(1),
                duration_ms: 1,
                timed_out: false,
                ok: false,
                log_excerpt: "Diff in src/lib.rs at line 4".into(),
            }],
        };
        assert_eq!(
            report_failures(&report).get("fmt").unwrap().iter().next(),
            Some(&OPAQUE_FAILURE.to_string())
        );
    }

    #[test]
    fn a_failure_already_in_the_baseline_is_not_new() {
        let base = set("test", &["a", "b"]);
        let current = set("test", &["a", "b"]);
        assert!(diff_against_baseline(&current, &base).is_empty());
    }

    #[test]
    fn a_failure_absent_from_the_baseline_is_new() {
        let base = set("test", &["a"]);
        let current = set("test", &["a", "b"]);
        assert_eq!(
            describe_failures(&diff_against_baseline(&current, &base)),
            vec!["test: b".to_string()]
        );
    }

    /// The case a count-based check waves through: same number of failures, but a different one.
    #[test]
    fn equal_counts_with_a_different_test_is_still_a_regression() {
        let base = set("test", &["a", "b"]);
        let current = set("test", &["a", "c"]);
        let new = diff_against_baseline(&current, &base);
        assert_eq!(describe_failures(&new), vec!["test: c".to_string()]);
    }

    /// A branch that only *fixes* things must pass. This is the case absolute-green got wrong:
    /// it blocks the work that repairs a red base.
    #[test]
    fn fixing_baseline_failures_yields_nothing_new() {
        let base = set("test", &["a", "b"]);
        let current = FailureSet::new();
        assert!(diff_against_baseline(&current, &base).is_empty());
    }

    /// Steps are separate namespaces — the same identity under a different step is a new fact.
    #[test]
    fn the_same_identity_under_a_different_step_is_new() {
        let base = set("test", &["a"]);
        let current = set("clippy", &["a"]);
        assert_eq!(
            describe_failures(&diff_against_baseline(&current, &base)),
            vec!["clippy: a".to_string()]
        );
    }

    /// No baseline at all (first run on an unknown base) means every failure is new — the gate
    /// stays fail-closed rather than defaulting to "probably fine".
    #[test]
    fn an_empty_baseline_treats_every_failure_as_new() {
        let current = set("test", &["a", "b"]);
        assert_eq!(diff_against_baseline(&current, &FailureSet::new()).len(), 1);
        assert_eq!(
            describe_failures(&diff_against_baseline(&current, &FailureSet::new())).len(),
            2
        );
    }
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
    async fn continues_after_required_failure_for_complete_report() {
        let dir = tempfile::tempdir().unwrap();
        let fail = if cfg!(windows) { "exit /B 1" } else { "exit 1" };
        let spec = PreflightSpec::new(
            "ship",
            vec![
                PreflightStep::new("pass", "echo hi"),
                PreflightStep::new("boom", fail),
                PreflightStep::new("later", "echo still-runs"),
            ],
        );
        let report = run_preflight(dir.path(), &spec).await.unwrap();
        assert!(
            !report.ok,
            "overall must stay fail-closed: {:?}",
            report.summary
        );
        assert_eq!(
            report.steps.len(),
            3,
            "must run later steps after a required failure"
        );
        assert!(report.steps[0].ok);
        assert!(!report.steps[1].ok);
        assert!(
            report.steps[2].ok,
            "later diagnostic/action step must still execute"
        );
        assert!(report.summary.contains("boom"));
        assert!(
            report.summary.contains("failed at step(s)"),
            "summary should list failed steps: {}",
            report.summary
        );
    }

    /// Staged ship bar: early format failure must not hide later test/clippy-style failures.
    #[tokio::test]
    async fn multi_required_failures_all_appear_in_report() {
        let dir = tempfile::tempdir().unwrap();
        let fail = if cfg!(windows) { "exit /B 1" } else { "exit 1" };
        let spec = PreflightSpec::new(
            "ship",
            vec![
                PreflightStep::new("fmt", fail),
                PreflightStep::new("compile", "echo ok"),
                PreflightStep::new("test", fail),
                PreflightStep::new("clippy", fail),
            ],
        );
        let report = run_preflight(dir.path(), &spec).await.unwrap();
        assert!(!report.ok);
        assert_eq!(report.steps.len(), 4, "all staged steps must run");
        let names: Vec<_> = report.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fmt", "compile", "test", "clippy"]);
        assert!(!report.steps[0].ok);
        assert!(report.steps[1].ok);
        assert!(!report.steps[2].ok);
        assert!(!report.steps[3].ok);
        let failed = report_failures(&report);
        assert!(failed.contains_key("fmt"), "{failed:?}");
        assert!(failed.contains_key("test"), "{failed:?}");
        assert!(failed.contains_key("clippy"), "{failed:?}");
        assert!(
            !failed.contains_key("compile"),
            "green compile must not appear in failures: {failed:?}"
        );
        assert!(
            report.summary.contains("fmt")
                && report.summary.contains("test")
                && report.summary.contains("clippy"),
            "summary should name every failed step: {}",
            report.summary
        );
    }

    /// Baseline differential still works when multiple staged steps fail.
    #[test]
    fn staged_multi_step_failures_preserve_baseline_diff() {
        let mut current = FailureSet::new();
        current.insert(
            "fmt".into(),
            [OPAQUE_FAILURE.to_string()].into_iter().collect(),
        );
        current.insert(
            "test".into(),
            ["gates::new_break".to_string()].into_iter().collect(),
        );
        current.insert(
            "clippy".into(),
            [OPAQUE_FAILURE.to_string()].into_iter().collect(),
        );

        let mut baseline = FailureSet::new();
        baseline.insert(
            "fmt".into(),
            [OPAQUE_FAILURE.to_string()].into_iter().collect(),
        );
        // clippy was already failing on base; test was green
        baseline.insert(
            "clippy".into(),
            [OPAQUE_FAILURE.to_string()].into_iter().collect(),
        );

        let new = diff_against_baseline(&current, &baseline);
        let described = describe_failures(&new);
        assert_eq!(
            described,
            vec!["test: gates::new_break".to_string()],
            "pre-existing fmt/clippy must be ignored; new test failure remains: {described:?}"
        );
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
        assert_eq!(names, ["fmt", "compile", "test", "clippy", "deny"]);
        assert!(
            spec.steps
                .iter()
                .any(|s| s.run.contains("cargo test --workspace"))
        );
        assert!(
            spec.steps
                .iter()
                .any(|s| s.run.contains("exclude liberado-webui") && s.run.contains("-D warnings"))
        );
        assert!(
            spec.steps
                .iter()
                .any(|s| s.name == "compile" && s.run.contains("cargo check"))
        );
    }

    #[test]
    fn resolve_ship_spec_none_for_unknown_project_without_config() {
        assert!(resolve_ship_spec(Some("notes"), None).is_none());
        assert!(resolve_ship_spec(None, None).is_none());
    }

    #[test]
    fn default_true_returns_true() {
        assert!(super::default_true());
    }

    #[test]
    fn default_constants_have_expected_values() {
        assert_eq!(super::DEFAULT_STEP_TIMEOUT_SECS, 45 * 60);
        assert_eq!(super::DEFAULT_LOG_CAP_BYTES, 16 * 1024);
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
    fn cap_log_truncation_at_multibyte_boundary_exercises_scan_loop() {
        // Two 3-byte chars = 6 bytes. Truncating to 5 bytes means start=1,
        // which falls inside the first multi-byte char. The loop must scan
        // forward to the next char boundary at byte 3.
        let s = "字字";
        let capped = cap_log(s, 5);
        assert!(capped.starts_with('…'));
        assert_eq!(capped.len(), 6); // 3-byte … + second 字 (3 bytes)
    }

    #[test]
    fn cap_log_one_byte_shorter_than_input_truncates() {
        // Input is 6 bytes, max is 5 → must truncate (not return as-is).
        assert_ne!(cap_log("abcdef", 5), "abcdef");
        assert!(cap_log("abcdef", 5).starts_with('…'));
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
        // Stage order: format action → compile → test+clippy diagnostics → deny.
        assert_eq!(names, ["fmt", "compile", "test", "clippy", "deny"]);
        for s in &steps {
            assert!(s.required, "{} must be required", s.name);
            assert!(!s.run.is_empty(), "{} must have a run command", s.name);
        }
        assert!(steps[1].run.contains("cargo check"));
        assert!(steps[2].run.contains("--no-fail-fast"));
    }

    #[tokio::test]
    async fn required_failure_still_runs_following_optional() {
        let dir = tempfile::tempdir().unwrap();
        let fail = if cfg!(windows) { "exit /B 1" } else { "exit 1" };
        let mut optional = PreflightStep::new("soft", fail);
        optional.required = false;
        let spec = PreflightSpec::new(
            "ship",
            vec![PreflightStep::new("hard-fail", fail), optional],
        );
        let report = run_preflight(dir.path(), &spec).await.unwrap();
        assert!(!report.ok, "required step failed, overall must be false");
        assert_eq!(
            report.steps.len(),
            2,
            "optional after a required failure must still run for a complete report"
        );
        assert!(!report.steps[0].ok);
        assert!(!report.steps[1].ok);
    }

    #[tokio::test]
    async fn optional_failure_does_not_fail_profile() {
        let dir = tempfile::tempdir().unwrap();
        let fail = if cfg!(windows) { "exit /B 1" } else { "exit 1" };
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

    #[tokio::test]
    async fn step_with_stdout_and_stderr_captures_both() {
        let dir = tempfile::tempdir().unwrap();
        // Produce both stdout and stderr so the combining branches are exercised.
        let cmd = if cfg!(windows) {
            "echo out && echo err 1>&2"
        } else {
            "echo out && echo err >&2"
        };
        let spec = PreflightSpec::new("ship", vec![PreflightStep::new("both", cmd)]);
        let report = run_preflight(dir.path(), &spec).await.unwrap();
        assert!(report.ok);
        let log = &report.steps[0].log_excerpt;
        assert!(log.contains("out"));
        assert!(log.contains("err"));
    }
}
