use super::derive_dispositions;
use crate::soften_pre_existing_test_failures;
use liberado_coder_core::{
    Disposition, Finding, FindingKind, NamedVerdict, PipelineResult, Verdict, VerdictStatus,
};

fn raised(pairs: &[(u32, &str)]) -> Vec<(u32, String)> {
    pairs.iter().map(|(a, s)| (*a, s.to_string())).collect()
}

/// An issue raised early and gone by the end was answered. Reporting it as open would train
/// a reader to skip the section, which is the only way this mechanism can actually fail.
#[test]
fn an_issue_absent_from_the_final_verdict_is_fixed() {
    let findings = derive_dispositions(&raised(&[(0, "the test does not bind")]), &[]);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].disposition, Disposition::Fixed);
    assert_eq!(findings[0].first_seen_attempt, 0);
}

/// The case the whole feature exists for: a finding still standing when the run filed.
#[test]
fn an_issue_in_the_final_verdict_is_outstanding() {
    let findings = derive_dispositions(
        &raised(&[(0, "still broken")]),
        &["still broken".to_string()],
    );
    assert_eq!(findings[0].disposition, Disposition::Outstanding);
}

/// A run that answered two of three complaints must show exactly that, not "clean" and not
/// "three problems".
#[test]
fn a_mixed_run_reports_both_kinds() {
    let findings =
        derive_dispositions(&raised(&[(0, "a"), (0, "b"), (1, "c")]), &["c".to_string()]);
    let outstanding: Vec<&str> = findings
        .iter()
        .filter(|f| f.disposition == Disposition::Outstanding)
        .map(|f| f.issue.as_str())
        .collect();
    assert_eq!(outstanding, vec!["c"]);
    assert_eq!(findings.len(), 3);
}

/// The same complaint restated across attempts is one complaint, dated to when it first
/// appeared — otherwise a stubborn issue inflates into a list and looks like several.
#[test]
fn a_repeated_issue_is_one_finding_dated_to_its_first_appearance() {
    let findings = derive_dispositions(
        &raised(&[
            (0, "same complaint"),
            (1, "same complaint"),
            (2, "same complaint"),
        ]),
        &["same complaint".to_string()],
    );
    assert_eq!(findings.len(), 1, "got {findings:?}");
    assert_eq!(findings[0].first_seen_attempt, 0);
    assert_eq!(findings[0].disposition, Disposition::Outstanding);
}

#[test]
fn a_run_with_no_findings_produces_none() {
    assert!(derive_dispositions(&[], &[]).is_empty());
}

// ── soften_pre_existing_test_failures ─────────────────────────────────

fn test_failure_log(test_names: &[&str]) -> String {
    let mut log = String::from("running 3 tests\n");
    for name in test_names {
        log.push_str(&format!("test {name} ... FAILED\n"));
    }
    log.push_str("test result: FAILED. 0 passed; 3 failed; 0 ignored\n");
    log
}

fn pipeline_with_test_verdict(
    test_status: VerdictStatus,
    test_log: Option<&str>,
) -> PipelineResult {
    PipelineResult {
        overall: if test_status == VerdictStatus::Pass {
            VerdictStatus::Pass
        } else {
            VerdictStatus::Fail
        },
        results: vec![
            NamedVerdict {
                id: "nonempty-diff".into(),
                kind: "git_nonempty_diff".into(),
                verdict: Verdict::pass("non-empty diff"),
            },
            NamedVerdict {
                id: "cargo-check".into(),
                kind: "command".into(),
                verdict: Verdict::pass("cargo exited 0"),
            },
            NamedVerdict {
                id: "cargo-test".into(),
                kind: "command".into(),
                verdict: if test_status == VerdictStatus::Pass {
                    Verdict::pass("cargo exited 0")
                } else {
                    Verdict::fail(
                        "cargo exited 101",
                        vec![Finding {
                            check_id: "cargo-test".into(),
                            kind: FindingKind::CommandFailed,
                            message: "cargo test exited 101".into(),
                            detail: None,
                        }],
                        test_log.map(|s| s.to_string()),
                    )
                },
            },
        ],
        combined_findings: if test_status == VerdictStatus::Pass {
            vec![]
        } else {
            vec![Finding {
                check_id: "cargo-test".into(),
                kind: FindingKind::CommandFailed,
                message: "cargo test exited 101".into(),
                detail: None,
            }]
        },
        combined_signature: None,
    }
}

fn bset(items: &[&str]) -> std::collections::BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// A failing cargo-test verifier whose failures all exist in the baseline is softened.
#[test]
fn pre_existing_test_failures_are_treated_as_passing() {
    let log = test_failure_log(&["foo::test_bar", "foo::test_baz"]);
    let pipeline = pipeline_with_test_verdict(VerdictStatus::Fail, Some(&log));
    assert!(!pipeline.is_pass(), "pipeline starts as failing");

    let baseline = bset(&["foo::test_bar", "foo::test_baz"]);
    let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);

    assert!(
        adjusted.is_pass(),
        "all failures are pre-existing; pipeline must pass"
    );
    assert_eq!(
        adjusted.results[2].verdict.status,
        VerdictStatus::Pass,
        "cargo-test verifier must be softened to Pass"
    );
}

/// New failures that do not appear in the baseline keep the pipeline failing.
#[test]
fn new_test_failures_are_not_softened() {
    let log = test_failure_log(&["foo::test_new_failure"]);
    let pipeline = pipeline_with_test_verdict(VerdictStatus::Fail, Some(&log));
    assert!(!pipeline.is_pass());

    let baseline = bset(&["foo::test_old_failure"]);
    let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);

    assert!(
        !adjusted.is_pass(),
        "only pre-existing failures should be softened"
    );
    assert_eq!(
        adjusted.results[2].verdict.status,
        VerdictStatus::Fail,
        "new failure must stay failing"
    );
}

/// A mix where some failures are pre-existing and some are new keeps the pipeline failing.
#[test]
fn mixed_pre_existing_and_new_failures_stay_failing() {
    let log = test_failure_log(&["foo::old", "foo::new"]);
    let pipeline = pipeline_with_test_verdict(VerdictStatus::Fail, Some(&log));

    let baseline = bset(&["foo::old"]);
    let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);

    assert!(
        !adjusted.is_pass(),
        "new failures with pre-existing ones must stay failing"
    );
}

/// An empty log excerpt with no parseable failures leaves the pipeline unchanged.
#[test]
fn a_test_failure_with_no_parseable_test_names_is_unchanged() {
    let pipeline =
        pipeline_with_test_verdict(VerdictStatus::Fail, Some("error: could not compile\n"));
    let baseline = bset(&["anything"]);
    let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);
    assert!(!adjusted.is_pass(), "opaque failure must not be forgiven");
    assert_eq!(adjusted.results[2].verdict.status, VerdictStatus::Fail,);
}

/// A pipeline with no cargo-test verifier is a no-op.
#[test]
fn absence_of_cargo_test_verifier_is_a_noop() {
    let pipeline = PipelineResult {
        overall: VerdictStatus::Fail,
        results: vec![NamedVerdict {
            id: "cargo-check".into(),
            kind: "command".into(),
            verdict: Verdict::fail("failed", vec![], None),
        }],
        combined_findings: vec![],
        combined_signature: None,
    };
    let baseline = bset(&["anything"]);
    let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);
    assert!(!adjusted.is_pass());
    assert_eq!(adjusted.results.len(), 1, "pipeline must be unchanged");
}

/// When a non-cargo-test verifier also fails, the overall stays failing even if test failures
/// are all pre-existing.
#[test]
fn another_verifier_failing_keeps_overall_failing() {
    let log = test_failure_log(&["foo::test_bar"]);
    let mut pipeline = pipeline_with_test_verdict(VerdictStatus::Fail, Some(&log));
    // Add a failed cargo-check too.
    pipeline.results[1] = NamedVerdict {
        id: "cargo-check".into(),
        kind: "command".into(),
        verdict: Verdict::fail("cargo check exited 1", vec![], None),
    };
    pipeline.overall = VerdictStatus::Fail;

    let baseline = bset(&["foo::test_bar"]);
    let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);

    assert!(
        !adjusted.is_pass(),
        "cargo-check still fails, so overall must be Fail"
    );
    assert_eq!(
        adjusted.results[2].verdict.status,
        VerdictStatus::Pass,
        "cargo-test was softened, but cargo-check was not"
    );
}
