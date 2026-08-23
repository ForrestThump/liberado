//! Split from `preflight.rs` for module-health boundaries.

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
