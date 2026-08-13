//! Path-parameterized MVL conformance oracle (layer 1).
//!
//! These tests drive [`liberado_test_support::mvl_oracle::run_mvl_conformance`] on **on-disk**
//! JSONL. The existing fixture suite in `mvl_conformance.rs` stays in place (layer 0).
//!
//! ## Foreign harness
//!
//! Write MVL JSONL at `$OUT/run.mvl.jsonl` (v1 envelope, append-flushed). Optionally write
//! `$OUT/run.execution.jsonl` with the same `run` / `turn` / `call_id`. Then:
//!
//! ```text
//! cargo run -p liberado-test-support --bin mvl-conformance -- \
//!   --mvl $OUT/run.mvl.jsonl \
//!   --execution $OUT/run.execution.jsonl \
//!   --expected-content-shown <call_id>=<path-to-ground-truth-bytes>
//! ```
//!
//! Liberado is not required. A converter from a native trace to MVL can sit in front of this
//! same binary. Liberado producer cases live in `mvl_e2e_liberado.rs`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use liberado_test_support::mvl_oracle::{
    ConformanceOpts, ConformanceRule, VerdictStatus, run_mvl_conformance,
};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/trace_contracts")
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("liberado-mvl-e2e-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write_scratch(name: &str, body: &str) -> PathBuf {
    let path = scratch_dir().join(name);
    let mut f =
        std::fs::File::create(&path).unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
    f.write_all(body.as_bytes()).unwrap();
    f.flush().unwrap();
    path
}

fn fixture_honesty() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("call-1".into(), "pub struct PathPolicy { … }".into());
    m.insert("call-2".into(), "ok".into());
    m
}

fn fixture_opts() -> ConformanceOpts {
    ConformanceOpts {
        execution_path: Some(fixtures_dir().join("sample.execution.jsonl")),
        expected_content_shown: fixture_honesty(),
        kill_after_seq: None,
    }
}

#[test]
fn fixture_paths_pass_all_eight_rules() {
    let mvl = fixtures_dir().join("sample.mvl.jsonl");
    assert!(
        mvl.is_file(),
        "layer 0 fixture must remain: {}",
        mvl.display()
    );
    let report = run_mvl_conformance(&mvl, &fixture_opts()).expect("oracle");
    assert_eq!(report.verdicts.len(), 8, "{report:?}");
    for rule in ConformanceRule::ALL {
        let v = report.verdict(rule).expect("rule present");
        assert_eq!(v.status, VerdictStatus::Pass, "{rule:?} {}", v.detail);
    }
}

#[test]
fn copied_fixture_on_a_new_path_is_not_a_hardcoded_reader() {
    // The oracle takes a path. Copying the sample to a new file proves we do not only
    // reopen fixtures/trace_contracts/sample.mvl.jsonl by name.
    let src = fixtures_dir().join("sample.mvl.jsonl");
    let body = std::fs::read_to_string(&src).unwrap();
    let dest = write_scratch("copied.mvl.jsonl", &body);
    assert_ne!(dest, src);
    let exec_src = fixtures_dir().join("sample.execution.jsonl");
    let exec_body = std::fs::read_to_string(&exec_src).unwrap();
    let exec_dest = write_scratch("copied.execution.jsonl", &exec_body);

    let mut opts = fixture_opts();
    opts.execution_path = Some(exec_dest);
    let report = run_mvl_conformance(&dest, &opts).expect("oracle");
    assert!(report.all_checked_passed(), "{report:?}");
    assert_eq!(
        report
            .verdict(ConformanceRule::Reconstruction)
            .unwrap()
            .status,
        VerdictStatus::Pass
    );
}

#[test]
fn existing_fixture_suite_files_are_not_deleted() {
    for name in ["sample.mvl.jsonl", "sample.execution.jsonl"] {
        let p = fixtures_dir().join(name);
        assert!(p.is_file(), "must not delete {}", p.display());
        assert!(std::fs::metadata(&p).unwrap().len() > 200);
    }
}

#[test]
fn seq_gap_fails_only_ordering() {
    let body = r#"
{"v":1,"type":"run_started","ts":"t","run":"r","seq":0,"harness":{"name":"x","version":"0"}}
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":1,"sha256":"c","tools":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":3,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
    let path = write_scratch("seq-gap.mvl.jsonl", body);
    let report = run_mvl_conformance(&path, &ConformanceOpts::default()).expect("oracle");
    let ordering = report.verdict(ConformanceRule::Ordering).unwrap();
    assert_eq!(ordering.status, VerdictStatus::Fail, "{}", ordering.detail);
    assert!(ordering.detail.contains("seq gap"), "{}", ordering.detail);
    assert_eq!(
        report
            .verdict(ConformanceRule::CrashSurvival)
            .unwrap()
            .status,
        VerdictStatus::Pass
    );
}

#[test]
fn crash_prefix_of_complete_lines_passes_crash_survival() {
    let src = std::fs::read_to_string(fixtures_dir().join("sample.mvl.jsonl")).unwrap();
    let prefix: String = src
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("\n");
    let path = write_scratch("crash-prefix.mvl.jsonl", &prefix);
    let report = run_mvl_conformance(&path, &ConformanceOpts::default()).expect("oracle");
    assert_eq!(
        report
            .verdict(ConformanceRule::CrashSurvival)
            .unwrap()
            .status,
        VerdictStatus::Pass,
        "{}",
        report
            .verdict(ConformanceRule::CrashSurvival)
            .unwrap()
            .detail
    );
    // Prefix is complete JSONL; reconstruction of present turns still works.
    assert_eq!(
        report
            .verdict(ConformanceRule::Reconstruction)
            .unwrap()
            .status,
        VerdictStatus::Pass
    );
}

#[test]
fn kill_after_seq_judges_the_durable_prefix() {
    let mvl = fixtures_dir().join("sample.mvl.jsonl");
    let opts = ConformanceOpts {
        kill_after_seq: Some(4),
        ..ConformanceOpts::default()
    };
    let report = run_mvl_conformance(&mvl, &opts).expect("oracle");
    let crash = report.verdict(ConformanceRule::CrashSurvival).unwrap();
    assert_eq!(crash.status, VerdictStatus::Pass, "{}", crash.detail);
    assert!(
        crash.detail.contains("kill_after_seq=4"),
        "{}",
        crash.detail
    );
}

#[test]
fn trailing_partial_fails_only_crash_survival() {
    let src = std::fs::read_to_string(fixtures_dir().join("sample.mvl.jsonl")).unwrap();
    let body = format!("{src}\n{{\"v\":1,\"type\":\"prompt\",\"partial\":");
    let path = write_scratch("trailing-partial.mvl.jsonl", &body);
    let report = run_mvl_conformance(&path, &ConformanceOpts::default()).expect("oracle");
    let crash = report.verdict(ConformanceRule::CrashSurvival).unwrap();
    assert_eq!(crash.status, VerdictStatus::Fail, "{}", crash.detail);
    assert!(crash.detail.contains("incomplete"), "{}", crash.detail);
    for rule in ConformanceRule::ALL {
        if rule == ConformanceRule::CrashSurvival {
            continue;
        }
        assert_eq!(
            report.verdict(rule).unwrap().status,
            VerdictStatus::Skipped,
            "{rule:?} should not be the only failure"
        );
    }
}

#[test]
fn offered_shrink_without_tools_changed_fails_only_withdrawal() {
    let body = r#"
{"v":1,"type":"run_started","ts":"t","run":"r","seq":0,"harness":{"name":"x","version":"0"}}
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":1,"sha256":"c","tools":[{"name":"a"},{"name":"b"}]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":2,"turn":0,"messages":{"mode":"full","items":[{"role":"user","content":"x"}]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":["a","b"],"params":{}}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":3,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":["a"],"params":{}}
"#;
    let path = write_scratch("no-withdrawal.mvl.jsonl", body);
    let report = run_mvl_conformance(&path, &ConformanceOpts::default()).expect("oracle");
    let w = report.verdict(ConformanceRule::WithdrawalVisible).unwrap();
    assert_eq!(w.status, VerdictStatus::Fail, "{}", w.detail);
    assert!(w.detail.contains("tools_changed"), "{}", w.detail);
    assert_eq!(
        report
            .verdict(ConformanceRule::Reconstruction)
            .unwrap()
            .status,
        VerdictStatus::Pass
    );
    assert_eq!(
        report.verdict(ConformanceRule::Ordering).unwrap().status,
        VerdictStatus::Pass
    );
}

#[test]
fn honesty_mismatch_fails_only_honesty() {
    let mvl = fixtures_dir().join("sample.mvl.jsonl");
    let mut expected = BTreeMap::new();
    expected.insert("call-1".into(), "NOT THE REAL BYTES".into());
    let opts = ConformanceOpts {
        execution_path: None,
        expected_content_shown: expected,
        kill_after_seq: None,
    };
    let report = run_mvl_conformance(&mvl, &opts).expect("oracle");
    let h = report.verdict(ConformanceRule::ToolHonesty).unwrap();
    assert_eq!(h.status, VerdictStatus::Fail, "{}", h.detail);
    assert!(
        h.detail.contains("content_shown != ground truth"),
        "{}",
        h.detail
    );
    assert_eq!(
        report
            .verdict(ConformanceRule::Reconstruction)
            .unwrap()
            .status,
        VerdictStatus::Pass
    );
}

#[test]
fn join_without_matching_call_id_fails_only_join() {
    let mvl = fixtures_dir().join("sample.mvl.jsonl");
    let exec = write_scratch(
        "orphan.execution.jsonl",
        r#"{"v":1,"type":"tool_started","ts":"t","run":"fixture-run-1","seq":0,"turn":0,"call_id":"orphan","name":"read_file"}
"#,
    );
    let opts = ConformanceOpts {
        execution_path: Some(exec),
        expected_content_shown: BTreeMap::new(),
        kill_after_seq: None,
    };
    let report = run_mvl_conformance(&mvl, &opts).expect("oracle");
    let j = report.verdict(ConformanceRule::JoinIntegrity).unwrap();
    assert_eq!(j.status, VerdictStatus::Fail, "{}", j.detail);
    assert!(j.detail.contains("orphan"), "{}", j.detail);
    assert_eq!(
        report
            .verdict(ConformanceRule::Reconstruction)
            .unwrap()
            .status,
        VerdictStatus::Pass
    );
}

#[test]
fn unrecoverable_system_hash_fails_system_prompt_rule() {
    let body = r#"
{"v":1,"type":"run_started","ts":"t","run":"r","seq":0,"harness":{"name":"x","version":"0"}}
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":1,"sha256":"c","tools":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":2,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"missing","text":null},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
    let path = write_scratch("no-system.mvl.jsonl", body);
    let report = run_mvl_conformance(&path, &ConformanceOpts::default()).expect("oracle");
    let s = report
        .verdict(ConformanceRule::SystemPromptRecoverable)
        .unwrap();
    assert_eq!(s.status, VerdictStatus::Fail, "{}", s.detail);
    assert!(s.detail.contains("not recoverable"), "{}", s.detail);
}

#[test]
fn foreign_invocation_is_documented_in_this_module() {
    let src = include_str!("mvl_e2e_oracle.rs");
    assert!(src.contains("--mvl $OUT/run.mvl.jsonl"));
    assert!(src.contains("mvl-conformance"));
    assert!(src.contains("run.execution.jsonl"));
}

/// Prevents the fixture suite from being replaced by this file.
#[test]
fn layer_zero_fixture_suite_still_exists() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mvl_conformance.rs");
    assert!(suite.is_file(), "do not replace {}", suite.display());
    let text = std::fs::read_to_string(&suite).unwrap();
    assert!(text.contains("mvl_fixture_reconstructs_every_turn"));
}
