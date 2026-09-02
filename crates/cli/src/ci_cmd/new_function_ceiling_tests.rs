//! Split from `ci_cmd.rs` for module-health boundaries.

use super::DELTA_REPORT;
use super::new_function_ceiling::enforce;
use serde_json::json;
use std::fs;
use tempfile::tempdir;

fn report(entries: serde_json::Value) -> serde_json::Value {
    json!({ "version": "0.4.3", "entries": entries })
}

fn entry(file: &str, function: &str, crap: f64, status: &str) -> serde_json::Value {
    json!({ "file": file, "function": function, "crap": crap, "status": status })
}

fn enforce_report(entries: serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".liberado")).unwrap();
    fs::write(temp.path().join(DELTA_REPORT), report(entries).to_string()).unwrap();
    enforce(temp.path(), "29.9")
}

#[test]
fn existing_baseline_entries_are_not_a_ceiling_failure() {
    enforce_report(json!([entry(
        "crates/cli/src/ci_cmd.rs",
        "check",
        42.0,
        "unchanged"
    )]))
    .unwrap();
}

#[test]
fn a_new_function_above_the_ceiling_is_reported() {
    let error = enforce_report(json!([entry(
        "crates/foo/src/lib.rs",
        "new_thing",
        42.0,
        "new"
    )]))
    .unwrap_err()
    .to_string();
    assert!(error.contains("new_thing"), "{error}");
}

#[test]
fn a_new_function_at_or_below_the_ceiling_is_allowed() {
    enforce_report(json!([
        entry("crates/foo/src/lib.rs", "ok", 29.9, "new"),
        entry("crates/foo/src/lib.rs", "also_ok", 20.0, "new"),
    ]))
    .unwrap();
}

#[test]
fn a_new_duplicate_identity_is_still_a_ceiling_failure() {
    let error = enforce_report(json!([
        entry("crates/foo/src/lib.rs", "Thing::run", 1.0, "unchanged"),
        entry("crates/foo/src/lib.rs", "Thing::run", 42.0, "new"),
    ]))
    .unwrap_err()
    .to_string();
    assert!(error.contains("Thing::run"), "{error}");
    assert!(error.contains("42.00"), "{error}");
}

#[test]
fn malformed_delta_entries_fail_closed() {
    let error = enforce_report(json!([{
        "file": "crates/foo/src/lib.rs",
        "function": "new_thing",
        "crap": 42.0
    }]))
    .unwrap_err()
    .to_string();
    assert!(error.contains("could not parse"), "{error}");
    assert!(error.contains("status"), "{error}");

    let error = enforce_report(json!([entry(
        "crates/foo/src/lib.rs",
        "new_thing",
        42.0,
        "future-status"
    )]))
    .unwrap_err()
    .to_string();
    assert!(error.contains("unknown variant"), "{error}");
}
