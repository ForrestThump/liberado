//! Split from `repair_feedback.rs`: kills the baseline campaign's survivors.
//!
//! Covers the hint table, the findings-less formatting branch, clip-window arithmetic,
//! marker recognition, classification arms, and the focus-block assembly.

use super::*;
use liberado_coder_core::{NamedVerdict, Verdict, VerdictStatus};

#[test]
fn every_class_has_its_distinct_hint() {
    let expectations: &[(FailureClass, &str)] = &[
        (FailureClass::NoChanges, "real workspace mutation"),
        (FailureClass::MissingPath, "Create the missing paths"),
        (FailureClass::ContentMismatch, "Edit the named files"),
        (FailureClass::CommandFailed, "Reproduce"),
        (FailureClass::CommandTimeout, "Speed up or simplify"),
        (FailureClass::EmptyDiff, "git status"),
        (FailureClass::CriticRevision, "each critic issue"),
        (FailureClass::Infrastructure, "operator"),
        (FailureClass::ValidationOther, "change approach"),
        (FailureClass::Other, "change approach"),
    ];
    for (class, needle) in expectations {
        assert!(
            class.repair_hint().contains(needle),
            "{class:?} hint must mention {needle:?}: {}",
            class.repair_hint()
        );
    }
}

/// With no combined findings, the per-result fallback must describe the failing verdicts.
#[test]
fn pipeline_without_findings_lists_failing_results() {
    let pipeline = PipelineResult {
        overall: VerdictStatus::Fail,
        results: vec![
            NamedVerdict {
                id: "paths".into(),
                kind: "paths_exist".into(),
                verdict: Verdict::fail("two paths missing", vec![], None),
            },
            NamedVerdict {
                id: "fmt".into(),
                kind: "command".into(),
                verdict: Verdict::pass("clean"),
            },
        ],
        combined_findings: vec![],
        combined_signature: Some("sig".into()),
    };
    let fb = format_pipeline_repair(&pipeline);
    assert!(
        fb.contains("- [paths_exist] paths: two paths missing"),
        "{fb}"
    );
    assert!(
        !fb.contains("fmt"),
        "passing results are noise for the repair role: {fb}"
    );
}

/// The package-marker window opens two lines ABOVE the marker. When the very first line
/// carries the marker, the clip must keep the head, not fall back to the tail.
#[test]
fn a_package_marker_on_the_first_line_keeps_the_head() {
    let mut log = vec!["error: test failed, to rerun pass `-p liberado-zeta`".to_string()];
    log.extend((1..20).map(|n| format!("ordinary line {n}")));
    let clipped = clip_log_excerpt(&log.join("\n"), 5);
    assert!(
        clipped.starts_with("error: test failed"),
        "head marker must pin the window to the top: {clipped}"
    );
    assert!(clipped.contains("ordinary line 1"), "{clipped}");
    assert!(!clipped.contains("ordinary line 19"), "{clipped}");
}

/// `could not compile` is recognised only by the package-marker rule; the generic
/// failure-marker rule has no needle for it. Its context window must survive clipping.
#[test]
fn could_not_compile_is_a_package_marker_with_context() {
    let mut log: Vec<String> = (0..10).map(|n| format!("filler {n}")).collect();
    // No `error:` prefix on purpose: this shape must be caught by the package-marker
    // rule alone, so the test distinguishes it from the generic failure markers.
    log.push("could not compile `zeta` (bin \"zeta\")".to_string());
    log.extend((11..30).map(|n| format!("filler {n}")));
    log.push("test zeta::tests::case ... FAILED".to_string());
    let clipped = clip_log_excerpt(&log.join("\n"), 6);
    assert!(
        clipped.contains("could not compile `zeta`"),
        "package-marker context must beat a distant generic marker: {clipped}"
    );
    assert!(!clipped.contains("filler 29"), "{clipped}");
}

#[test]
fn every_failure_marker_needle_recognises_its_own_shape() {
    let shapes = [
        "test zeta::tests::case ... FAILED",
        "error[E0425]: cannot find value `foo` in this scope",
        "error: linker `cc` not found",
        "panicked at crates/zeta/src/lib.rs:7:5:",
        "test result: failed. 0 passed; 1 failed; 0 ignored",
    ];
    for line in shapes {
        assert!(is_failure_marker(line), "must recognise: {line}");
    }
    assert!(!is_failure_marker("running 12 tests"));
}

#[test]
fn result_kind_arms_route_without_combined_findings() {
    let kind_class = [
        ("paths_exist", FailureClass::MissingPath),
        ("paths_absent", FailureClass::MissingPath),
        ("content_contains", FailureClass::ContentMismatch),
        ("command", FailureClass::CommandFailed),
    ];
    for (kind, class) in kind_class {
        let pipeline = PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![NamedVerdict {
                id: "v".into(),
                kind: kind.into(),
                verdict: Verdict::fail("boom", vec![], None),
            }],
            combined_findings: vec![],
            combined_signature: None,
        };
        assert_eq!(classify_pipeline(&pipeline), class, "kind {kind}");
    }
}

#[test]
fn error_classification_arms() {
    assert_eq!(
        classify_error(&CoderError::NoChanges),
        FailureClass::NoChanges
    );
    assert_eq!(
        classify_error(&CoderError::Validation("command timed out".into())),
        FailureClass::CommandTimeout,
        "Validation messages route through the generic rule table"
    );
    assert_eq!(
        classify_error(&CoderError::Backend("linker exploded".into())),
        FailureClass::Other,
        "a Backend error without 'critic' is not a critic revision"
    );
}

#[test]
fn marked_validation_messages_pass_through_untouched() {
    let marked = "FAILURE_CLASS: command_timeout\nFINDINGS:\n- timed out";
    let err = CoderError::Validation(marked.into());
    assert_eq!(format_error_feedback(&err), marked);
}

#[test]
fn unmarked_validation_messages_get_the_full_envelope() {
    let fb = format_error_feedback(&CoderError::Validation("command timed out".into()));
    assert!(fb.contains("FAILURE_CLASS: command_timeout"), "{fb}");
    assert!(fb.contains("FAILURE_SIGNATURE: validation:"), "{fb}");
}

#[test]
fn the_signature_is_a_sha256_of_the_message() {
    let fb = format_error_feedback(&CoderError::Validation("missing path: x".into()));
    let sig = fb
        .lines()
        .find_map(|l| l.strip_prefix("FAILURE_SIGNATURE: validation:"))
        .expect("signature line");
    assert_eq!(sig.len(), 64, "sha256 hex digest length");
    assert!(
        sig.chars().all(|c| c.is_ascii_hexdigit()),
        "digest must be hex: {sig}"
    );
}

#[test]
fn a_single_prior_attempt_does_not_list_earlier_attempts() {
    let prior = vec![format_error_feedback(&CoderError::NoChanges)];
    let block = repair_focus_block(&prior).unwrap();
    assert!(
        !block.contains("Earlier attempts"),
        "one attempt has no earlier attempts: {block}"
    );
}

#[test]
fn earlier_attempts_never_include_the_latest_failure() {
    let prior = vec![
        format_error_feedback(&CoderError::NoChanges),
        "FAILURE_CLASS: infrastructure\nDETAIL: disk full mid-build".into(),
    ];
    let block = repair_focus_block(&prior).unwrap();
    assert!(block.contains("attempt 1"), "{block}");
    assert!(
        !block.contains("attempt 2:"),
        "the latest failure must not be re-listed as an earlier attempt: {block}"
    );
}

#[test]
fn earlier_attempts_quote_each_prior_first_line() {
    let prior = vec![
        "FAILURE_CLASS: no_changes\nDETAIL: nothing written".into(),
        "FAILURE_CLASS: missing_path\nDETAIL: src/main.rs absent".into(),
        "FAILURE_CLASS: infrastructure\nDETAIL: disk full".into(),
    ];
    let block = repair_focus_block(&prior).unwrap();
    assert!(
        block.contains("- attempt 1: FAILURE_CLASS: no_changes"),
        "{block}"
    );
    assert!(
        block.contains("- attempt 2: FAILURE_CLASS: missing_path"),
        "{block}"
    );
}
