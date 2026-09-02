//! New-function CRAP ceiling.
//!
//! cargo-crap `--fail-above` scores every function, including baseline entries that still
//! sit in the 30–50 band. Liberado therefore reads cargo-crap's delta report and applies the
//! ceiling only to entries its move-aware matcher classifies as new. Existing scores stay
//! under the per-function ratchet.

use serde::Deserialize;
use std::path::Path;

use super::{
    BASELINE_FILE, CRAP_CEILING, CRAP_REGRESSION_MIN, CRAP_REPORT_THRESHOLD, CiLog, DELTA_REPORT,
    LCOV_FILE, baseline_has_entries, emit_crap_failure, relativize_json_file, run_cmd,
};

#[derive(Deserialize)]
struct DeltaReport {
    entries: Vec<DeltaEntry>,
}

#[derive(Deserialize)]
struct DeltaEntry {
    file: String,
    function: String,
    crap: f64,
    status: DeltaStatus,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DeltaStatus {
    New,
    Regressed,
    Improved,
    Unchanged,
    Moved,
}

pub(super) fn compare(
    log: &CiLog,
    fail_regression: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let fail_above = !baseline_has_entries(&log.root.join(BASELINE_FILE));
    run_cmd(log, "cargo", &compare_args(fail_regression, fail_above))
        .map_err(|error| emit_crap_failure(fail_regression, error))?;
    apply_ceiling_when_baseline_exists(log, fail_above)
}

fn apply_ceiling_when_baseline_exists(
    log: &CiLog,
    fail_above: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if fail_above {
        return Ok(());
    }
    relativize_json_file(&log.root, DELTA_REPORT)?;
    enforce(&log.root, CRAP_CEILING).map_err(|error| emit_crap_failure(false, error))
}

pub(super) fn compare_args(fail_regression: bool, fail_above: bool) -> Vec<&'static str> {
    let mut args = vec!["crap", "--workspace", "--lcov", LCOV_FILE];
    if fail_above {
        args.extend_from_slice(&["--fail-above", "--threshold", CRAP_CEILING]);
    } else {
        args.extend_from_slice(&[
            "--baseline",
            BASELINE_FILE,
            "--format",
            "json",
            "--threshold",
            CRAP_REPORT_THRESHOLD,
            "--sort",
            "file",
            "--output",
            DELTA_REPORT,
        ]);
    }
    append_regression_args(&mut args, fail_regression);
    args
}

fn append_regression_args(args: &mut Vec<&'static str>, fail_regression: bool) {
    if fail_regression {
        args.extend_from_slice(&["--min", CRAP_REGRESSION_MIN, "--fail-regression"]);
    }
}

pub(super) fn enforce(root: &Path, ceiling: &str) -> Result<(), Box<dyn std::error::Error>> {
    let ceiling: f64 = ceiling.parse()?;
    let delta = load_report(&root.join(DELTA_REPORT))?;
    let offenders = functions_above_ceiling(&delta, ceiling);
    if offenders.is_empty() {
        return Ok(());
    }
    let mut lines = vec![format!(
        "new function CRAP ceiling is {ceiling}; cargo-crap classified these functions as new and they scored above it:"
    )];
    for (file, function, crap) in offenders {
        lines.push(format!("  {crap:.2}  {file} :: {function}"));
    }
    Err(lines.join("\n").into())
}

fn functions_above_ceiling(report: &DeltaReport, ceiling: f64) -> Vec<(String, String, f64)> {
    let mut offenders = Vec::new();
    for entry in &report.entries {
        if entry.status != DeltaStatus::New {
            continue;
        }
        if entry.crap > ceiling {
            offenders.push((entry.file.clone(), entry.function.clone(), entry.crap));
        }
    }
    offenders.sort_by(|left, right| {
        right
            .2
            .partial_cmp(&left.2)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    offenders
}

fn load_report(path: &Path) -> Result<DeltaReport, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("could not parse {}: {error}", path.display()).into())
}
