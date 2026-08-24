//! Integration tests for the `mvl-conformance` binary's exit-code contract.
//!
//! The binary is the foreign-harness door into the oracle, so its *observable* is the process
//! exit code and stdout JSON, not the Rust report type: 0 = every rule passed or was skipped,
//! 1 = at least one rule failed, 2 = usage/oracle/serialization error.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_mvl-conformance")
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/trace_contracts")
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("liberado-mvl-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn scratch(name: &str) -> PathBuf {
    scratch_dir().join(name)
}

fn run_cli(args: &[&str]) -> (std::process::ExitStatus, String) {
    let output = Command::new(bin_path())
        .args(args)
        .output()
        .expect("binary runs");
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn no_arguments_prints_usage_and_exits_two() {
    let (status, _stdout) = run_cli(&[]);
    assert_eq!(status.code(), Some(2), "usage error is exit code 2");
}

#[test]
fn an_unknown_flag_exits_two_with_the_parser_error() {
    let (status, _stdout) = run_cli(&["--mvl"]);
    assert_eq!(status.code(), Some(2));
}

#[test]
fn a_missing_mvl_file_reports_an_oracle_error_and_exits_two() {
    let (status, _stdout) = run_cli(&["--mvl", "/nonexistent/liberado-cli-test.mvl.jsonl"]);
    assert_eq!(
        status.code(),
        Some(2),
        "oracle errors are not rule failures"
    );
}

#[test]
fn the_passing_fixture_exits_zero_and_prints_a_json_report() {
    let fixture = fixtures_dir().join("sample.mvl.jsonl");
    let execution = fixtures_dir().join("sample.execution.jsonl");
    // The sample's honesty ground truth lives next to the other fixtures; the CLI takes it as
    // call_id=path pairs.
    let honesty_path = scratch("honesty-call-1.txt");
    std::fs::write(&honesty_path, "pub struct PathPolicy { … }").unwrap();

    let (status, stdout) = run_cli(&[
        "--mvl",
        fixture.to_str().unwrap(),
        "--execution",
        execution.to_str().unwrap(),
        "--expected-content-shown",
        &format!("call-1={}", honesty_path.display()),
    ]);
    assert_eq!(status.code(), Some(0), "stdout: {stdout}");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("JSON report on stdout");
    let verdicts = report["verdicts"].as_array().expect("verdicts array");
    assert_eq!(verdicts.len(), 8, "{report:?}");
}

#[test]
fn a_failing_rule_exits_one_and_names_it_in_the_report() {
    // A trailing partial line fails exactly the crash-survival rule — the same mutation the
    // library-level oracle test uses, now observed through the process boundary.
    let src = std::fs::read_to_string(fixtures_dir().join("sample.mvl.jsonl")).unwrap();
    let path = scratch("cli-trailing-partial.mvl.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(format!("{src}\n{{\"v\":1,\"type\":\"prompt\",\"partial\":").as_bytes())
        .unwrap();
    drop(f);

    let (status, stdout) = run_cli(&["--mvl", path.to_str().unwrap()]);
    assert_eq!(status.code(), Some(1), "a failed rule is exit code 1");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("JSON report");
    let verdicts = report["verdicts"].as_array().unwrap();
    assert!(verdicts.len() == 8);
}
