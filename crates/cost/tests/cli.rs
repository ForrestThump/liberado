//! CLI integration tests for the `liberado-cost` binary.
//!
//! `main` is a thin dispatch shell over the lib functions, and the binary is never executed by
//! the unit suite, so its arms sat at 0% coverage. These tests run the real binary via
//! `CARGO_BIN_EXE_liberado-cost` and exercise each dispatch arm end-to-end: default Report,
//! --json, --topology error, ProvenanceRatio, DelegationCost, and the clap usage error.

use std::fs;
use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_liberado-cost");

/// One valid journal record — same shape the provider writer emits (see journal_shape.rs).
fn journal_line() -> &'static str {
    r#"{"ts_ms":1754000000123,"correlation":"chat-delegate-01KZ0JQJ5V359744Y3Q2M5RGXC","role":"orchestrator","model":"deepseek/deepseek-v4-pro","kind":"llm_call","wall_ms":20531,"ttft_ms":1204,"prompt_tokens":24455,"completion_tokens":1007,"total_tokens":25462,"cached_prompt_tokens":20736,"finish":"stop","tool_calls":3,"streamed":true,"repeat_calls":2}"#
}

/// A data dir the report and delegation-cost arms can read: journal + empty dispatches dir.
fn data_dir_with_journal(temp: &Path) {
    fs::create_dir_all(temp.join("latency")).unwrap();
    fs::write(
        temp.join("latency").join("events.jsonl"),
        format!("{}\n", journal_line()),
    )
    .unwrap();
    fs::create_dir_all(temp.join("dispatches")).unwrap();
}

#[test]
fn report_without_journal_is_an_empty_success() {
    let temp = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .arg("--data-dir")
        .arg(temp.path())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn report_with_corrupt_journal_fails_with_exit_1() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("latency")).unwrap();
    fs::write(
        temp.path().join("latency").join("events.jsonl"),
        "this is not json\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .arg("--data-dir")
        .arg(temp.path())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("liberado-cost:"), "{stderr}");
}

#[test]
fn report_with_journal_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    data_dir_with_journal(temp.path());
    let out = Command::new(BIN)
        .arg("--data-dir")
        .arg(temp.path())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn report_json_is_parseable_and_counts_the_journal() {
    let temp = tempfile::tempdir().unwrap();
    data_dir_with_journal(temp.path());
    let out = Command::new(BIN)
        .arg("--data-dir")
        .arg(temp.path())
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["event_count"], 1);
}

#[test]
fn report_with_missing_topology_fails_with_exit_1() {
    let temp = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .arg("--topology")
        .arg(temp.path().join("no-such-topology.toml"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("liberado-cost:"), "{stderr}");
}

#[test]
fn provenance_ratio_without_sessions_is_an_empty_success() {
    let temp = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .arg("--data-dir")
        .arg(temp.path())
        .arg("provenance-ratio")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("nothing to report"), "{stdout}");
}

#[test]
fn delegation_cost_with_journal_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    data_dir_with_journal(temp.path());
    let out = Command::new(BIN)
        .arg("--data-dir")
        .arg(temp.path())
        .arg("delegation-cost")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn unknown_subcommand_is_a_usage_error() {
    let out = Command::new(BIN).arg("frobnicate").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unrecognized subcommand"), "{stderr}");
}
