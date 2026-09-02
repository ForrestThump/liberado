//! New-function CRAP ceiling.
//!
//! cargo-crap `--fail-above` scores every function, including baseline entries that still
//! sit in the 30–50 band. Liberado therefore applies the ceiling only to functions that are
//! not in `crap-baseline.json`. Existing scores stay under the per-function ratchet.

use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

use super::{
    BASELINE_FILE, CRAP_CEILING, CRAP_REGRESSION_MIN, CURRENT_REPORT, CiLog, LCOV_FILE,
    baseline_has_entries, emit_crap_failure, run_cmd, write_crap_json,
};

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
    write_crap_json(log, CURRENT_REPORT)?;
    enforce(&log.root, CRAP_CEILING).map_err(|error| emit_crap_failure(false, error))
}

pub(super) fn compare_args(fail_regression: bool, fail_above: bool) -> Vec<&'static str> {
    let mut args = vec!["crap", "--workspace", "--lcov", LCOV_FILE];
    if fail_above {
        args.extend_from_slice(&["--fail-above", "--threshold", CRAP_CEILING]);
    }
    append_regression_args(&mut args, fail_regression);
    args
}

fn append_regression_args(args: &mut Vec<&'static str>, fail_regression: bool) {
    if fail_regression {
        args.extend_from_slice(&[
            "--min",
            CRAP_REGRESSION_MIN,
            "--baseline",
            BASELINE_FILE,
            "--fail-regression",
        ]);
    }
}

pub(super) fn enforce(root: &Path, ceiling: &str) -> Result<(), Box<dyn std::error::Error>> {
    let ceiling: f64 = ceiling.parse()?;
    let current = load_report(&root.join(CURRENT_REPORT))?;
    let baseline = load_report(&root.join(BASELINE_FILE))?;
    let offenders = functions_above_ceiling(&current, &baseline, ceiling);
    if offenders.is_empty() {
        return Ok(());
    }
    let mut lines = vec![format!(
        "new function CRAP ceiling is {ceiling}; these functions are not in {BASELINE_FILE} and scored above it:"
    )];
    for (file, function, crap) in offenders {
        lines.push(format!("  {crap:.2}  {file} :: {function}"));
    }
    Err(lines.join("\n").into())
}

pub(super) fn functions_above_ceiling(
    current: &Value,
    baseline: &Value,
    ceiling: f64,
) -> Vec<(String, String, f64)> {
    let known = entry_keys(baseline);
    let mut offenders = Vec::new();
    for entry in entries(current) {
        let Some((file, function, crap)) = entry_tuple(entry) else {
            continue;
        };
        if known.contains(&(file.clone(), function.clone())) {
            continue;
        }
        if crap > ceiling {
            offenders.push((file, function, crap));
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

fn load_report(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("could not parse {}: {error}", path.display()).into())
}

fn entries(report: &Value) -> &[Value] {
    report
        .get("entries")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn entry_keys(report: &Value) -> HashSet<(String, String)> {
    entries(report)
        .iter()
        .filter_map(|entry| {
            let file = entry.get("file")?.as_str()?.to_owned();
            let function = entry.get("function")?.as_str()?.to_owned();
            Some((file, function))
        })
        .collect()
}

fn entry_tuple(entry: &Value) -> Option<(String, String, f64)> {
    Some((
        entry.get("file")?.as_str()?.to_owned(),
        entry.get("function")?.as_str()?.to_owned(),
        entry.get("crap")?.as_f64()?,
    ))
}
