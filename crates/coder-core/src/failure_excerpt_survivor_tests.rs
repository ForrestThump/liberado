//! Split from `failure_excerpt.rs` for module-health boundaries.

//! Boundary and classification tests for the failure extractor. Each assertion
//! was verified to fail under the mutant it targets.

use super::*;

const FAIL: &str = "test cases::a ... FAILED";

#[test]
fn cap_zero_means_uncapped_and_exact_fit_adds_no_more_marker() {
    let log = format!("{FAIL}\nsecond\nthird\n");
    // max_lines == 0 is "no cap": everything survives.
    assert_eq!(extract_failures_capped(&log, 0, None), FAIL);

    // Exactly at the cap: truncate must not fire, so no "... 0 more" line.
    let log = format!("{FAIL}\n{FAIL}\n");
    let out = extract_failures_capped(&log, 2, None);
    assert_eq!(out.lines().count(), 2);
    assert!(!out.contains("more matching lines"), "{out}");
}

#[test]
fn over_the_cap_names_the_surplus_count_exactly() {
    let log = format!("{FAIL}\n{FAIL}\n{FAIL}\n{FAIL}\n{FAIL}\n");
    let out = extract_failures_capped(&log, 2, Some("full.log"));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out:?}");
    assert_eq!(lines[2], "… 3 more matching lines in full.log");

    let out = extract_failures_capped(&log, 2, None);
    assert!(
        out.lines()
            .nth(2)
            .unwrap()
            .ends_with("3 more matching lines")
    );
}

#[test]
fn diagnostic_context_extends_only_over_diagnostic_lines() {
    // rustc-style block: failure, span, gutter, note — then a blank prose line.
    let log = "error[E0308]: mismatched types\n\
                   --> crates/x/src/lib.rs:10:5\n\
                   10 |     let x: u8 = y;\n\
                   = note: expected u8, found u16\n\
                   prose paragraph that is not diagnostics\n";
    let out = extract_failures(log);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "error[E0308]: mismatched types");
    assert!(lines.iter().any(|l| l.contains("-->")), "{out:?}");
    assert!(
        lines.iter().any(|l| l.contains("|")),
        "source gutter lines are diagnostic context: {out:?}"
    );
    assert!(
        !out.contains("prose paragraph"),
        "non-diagnostic tail stays out: {out:?}"
    );

    // A "word | word" line has no numeric gutter and is not context.
    let log = "error: bad\nheader | trailer\n";
    let out = extract_failures(log);
    assert_eq!(out, "error: bad", "{out:?}");

    // The context window is bounded: at most 7 lines past the anchor. Nine
    // diagnostics follow, so exactly seven join the excerpt and lines 9-10 stay
    // out — an off-by-one in either bound shows up here.
    let mut parts = vec![FAIL.to_string()];
    for i in 1..=9 {
        parts.push(format!("--> context/line{i}:5"));
    }
    parts.push("prose tail".into());
    let out = extract_failures(&parts.join("\n"));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines.len(),
        8,
        "anchor + 7 context lines max, got {lines:?}"
    );
    assert!(lines[7].contains("line7"), "{lines:?}");
    assert!(!out.contains("line8"), "{out:?}");
}

#[test]
fn ansi_escapes_are_stripped_before_matching() {
    let log = "\u{1b}[31mtest cases::a ... FAILED\u{1b}[0m\nplain after\n";
    let out = extract_failures(log);
    assert!(out.starts_with("test cases::a ... FAILED"), "{out:?}");
    assert!(!out.contains('\u{1b}'));
}

#[test]
fn every_documented_failure_shape_is_matched() {
    for line in [
        "test a::b ... FAILED",
        "error[E0382]: borrow",
        "error: unimplemented",
        "panicked at 'x', src/y.rs:1",
        "test result: failed. 1 passed",
        "could not compile liberado-x",
        "Cyclomatic regressed 9 -> 12",
        "crap check failed for f",
        // CRAP table row: box-drawing with a + delta and no NEW marker.
        "▲ ┆ 156.0 ┆ +24.0 ┆ 12 ┆ f ┆ file.rs",
    ] {
        assert!(
            is_failure_line(line),
            "documented failure shape not matched: {line:?}"
        );
    }
}
