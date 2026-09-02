//! Split from `ci_cmd.rs` for module-health boundaries.

use super::new_function_ceiling::{enforce, functions_above_ceiling};
use super::{BASELINE_FILE, CURRENT_REPORT};
use serde_json::json;
use std::fs;
use tempfile::tempdir;

fn report(entries: serde_json::Value) -> serde_json::Value {
    json!({ "version": "0.4.3", "entries": entries })
}

fn entry(file: &str, function: &str, crap: f64) -> serde_json::Value {
    json!({ "file": file, "function": function, "crap": crap })
}

#[test]
fn existing_baseline_entries_are_not_a_ceiling_failure() {
    let current = report(json!([entry("crates/cli/src/ci_cmd.rs", "check", 42.0)]));
    let baseline = current.clone();
    assert!(functions_above_ceiling(&current, &baseline, 29.9).is_empty());
}

#[test]
fn a_new_function_above_the_ceiling_is_reported() {
    let current = report(json!([
        entry("crates/cli/src/ci_cmd.rs", "check", 42.0),
        entry("crates/foo/src/lib.rs", "new_thing", 42.0),
    ]));
    let baseline = report(json!([entry("crates/cli/src/ci_cmd.rs", "check", 42.0)]));
    let offenders = functions_above_ceiling(&current, &baseline, 29.9);
    assert_eq!(offenders.len(), 1);
    assert_eq!(offenders[0].0, "crates/foo/src/lib.rs");
    assert_eq!(offenders[0].1, "new_thing");
}

#[test]
fn a_new_function_at_or_below_the_ceiling_is_allowed() {
    let current = report(json!([
        entry("crates/foo/src/lib.rs", "ok", 29.9),
        entry("crates/foo/src/lib.rs", "also_ok", 20.0),
    ]));
    let baseline = report(json!([]));
    assert!(functions_above_ceiling(&current, &baseline, 29.9).is_empty());
}

#[test]
fn enforce_reads_the_current_and_baseline_reports() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join(".liberado")).unwrap();
    fs::write(
        root.join(CURRENT_REPORT),
        report(json!([entry("crates/foo/src/lib.rs", "new_thing", 42.0)])).to_string(),
    )
    .unwrap();
    fs::write(root.join(BASELINE_FILE), report(json!([])).to_string()).unwrap();
    let error = enforce(root, "29.9").unwrap_err().to_string();
    assert!(error.contains("new_thing"), "{error}");
    assert!(error.contains("42.00"), "{error}");
}
