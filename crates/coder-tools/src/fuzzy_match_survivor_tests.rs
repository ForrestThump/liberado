//! Survivor tests for `fuzzy_match.rs`'s private helpers — wired as a child
//! module so `super::*` reaches the private functions directly.

use super::*;

#[test]
fn indent_unit_is_the_smallest_positive_step() {
    // indents [0, 2]: steps are [0, 2], so the unit is 2 and the deeper line
    // sits one level below the shallowest. A zero step must never win.
    let lines = ["a", "    b"];
    let depths = relative_indent_depths(&lines);
    assert_eq!(depths, vec![0, 1], "{depths:?}");
}

#[test]
fn blank_lines_report_depth_zero() {
    // min_indent is 4 here; an empty line has no leading whitespace, so it
    // must short-circuit to 0 rather than underflow on `indent - min`.
    let lines = ["    a", "", "    b"];
    let depths = relative_indent_depths(&lines);
    assert_eq!(
        depths[1], 0,
        "a blank line carries no structure: {depths:?}"
    );
}

#[test]
fn depth_subtracts_the_block_minimum_not_adds_it() {
    // indents [2, 6]: unit 4, depths [0, 1]. Adding the minimum instead of
    // subtracting yields 2 for the deep line.
    let lines = ["  a", "      b"];
    let depths = relative_indent_depths(&lines);
    assert_eq!(depths, vec![0, 1], "{depths:?}");
}

#[test]
fn line_offsets_advance_by_line_length_plus_newline() {
    assert_eq!(line_offsets("ab\ncd\n"), vec![0, 3, 6]);
    assert_eq!(line_offsets("x"), vec![0]);
}

/// Exactly two windows clear the bar and neither dominates: the count the
/// ambiguity outcome reports must be exactly the above-threshold windows,
/// not every window scanned.
#[test]
fn ambiguity_count_counts_only_windows_above_threshold() {
    use MatchOutcome::*;
    // Two identical sites (tab-indented so the anchor cannot exact-match),
    // plus a third window nothing like them.
    // No trailing newline: three windows total - two score 1.0, the filler
    // scores far below the bar. An inverted above/below count would see only
    // ONE window and wrongly resolve via the unique-candidate branch.
    let content = "\tx\n\tx\nqqqqqqqqqqqqq";
    match find_match(content, "  x", true, DEFAULT_THRESHOLD) {
        Ambiguous { count, .. } => assert_eq!(count, 2, "only the two real candidates"),
        other => panic!("expected ambiguity, got {other:?}"),
    }
}

/// At a custom threshold a clearly dominant candidate wins even though a
/// weaker one also cleared the bar - the dominance ladder is load-bearing.
#[test]
fn a_dominant_candidate_wins_at_a_custom_threshold() {
    // Geometry (normalized lines carry a 2-char depth prefix):
    //   target vs site A: one char off   -> 41/42 = 0.976 (>= 0.97 dominant floor)
    //   target vs site B: five chars off -> 37/42 = 0.881 (>= 0.85 threshold)
    //   gap = 0.095 >= 0.08, so A dominates B and must be chosen.
    let target = "fn aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaxy() {}";
    let site_a = "fn aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaqy() {}";
    let site_b = "fn aaaaaaaaaaaaaaaaaaaaaaaaaabyby() {}";
    let content = format!("{site_a}\n{site_b}\nlet unrelated_filler_line = 7;\n");
    match find_match(&content, target, true, 0.85) {
        MatchOutcome::Fuzzy(m) => {
            assert_eq!(m.start_line, 1, "site A wins: {m:?}");
            assert!(m.confidence >= 0.97, "{}", m.confidence);
        }
        other => panic!("dominance must resolve in favour of the best site: {other:?}"),
    }
}

#[test]
fn reindent_shifts_every_line_by_the_anchor_delta() {
    // The anchor sat four columns deeper in reality than the model thought;
    // the whole replacement shifts left by four, per line.
    let old = "fn a() {}";
    let actual = "    fn a() {}";
    let new = "        one();\n        two();";
    // The anchor sat four columns deeper than the model thought, so every
    // replacement line shifts right by four.
    let out = adjust_indentation(old, actual, new);
    assert_eq!(out, "            one();\n            two();", "{out:?}");

    // Shallow lines shift by the same delta without going anywhere odd.
    let new_shallow = "one();\n        two();";
    let out2 = adjust_indentation(old, actual, new_shallow);
    assert_eq!(out2, "    one();\n            two();", "{out2:?}");
}

// ── edit distance ───────────────────────────────────────────────────────────

/// Golden Levenshtein distances covering insertion runs, transpositions,
/// substitutions, and the trivial edges.
#[test]
fn levenshtein_golden_pairs() {
    assert_eq!(levenshtein("", ""), 0);
    assert_eq!(levenshtein("", "x"), 1);
    assert_eq!(levenshtein("abc", ""), 3);
    assert_eq!(levenshtein("kitten", "sitting"), 3);
    assert_eq!(levenshtein("flaw", "lawn"), 2);
    assert_eq!(levenshtein("ab", "ba"), 2);
    assert_eq!(levenshtein("abcdef", "abcdef"), 0);
}

// ── window matching ─────────────────────────────────────────────────────────

/// A target taller than the content cannot match anywhere.
#[test]
fn taller_target_is_not_found() {
    let outcome = find_match("only\none", "a\nb\nc", true, 0.5);
    assert!(matches!(outcome, MatchOutcome::NotFound { closest: None }));
}

/// An equal-height near miss is found fuzzily: the window loop must still
/// run when lengths match exactly.
#[test]
fn equal_height_near_miss_matches_fuzzily() {
    let outcome = find_match("alpha\nbeta", "alpha\nbets", true, 0.6);
    assert!(
        matches!(outcome, MatchOutcome::Fuzzy(_)),
        "expected a fuzzy hit: {outcome:?}"
    );
}

/// Two equally-scoring windows stay ambiguous and report the *first* one as
/// best: strict improvement is what keeps the earliest window ahead.
#[test]
fn tied_windows_report_first_best_and_stay_ambiguous() {
    use MatchOutcome::*;
    let content = "abd\nQQQQQQQ\nabd";
    match find_match(content, "abc", true, 0.6) {
        Ambiguous { count, best } => {
            assert_eq!(count, 2, "{best:?}");
            assert_eq!(best.start_line, 1, "earliest window wins ties");
            assert_eq!(best.actual_text, "abd");
        }
        other => panic!("expected ambiguity across two windows: {other:?}"),
    }
}

/// An offset that is not a multiple of the unit separates the ceiling
/// branch from the round-to-nearest one: 4 over unit 3 rounds to 1.
#[test]
fn odd_unit_depths_round_down_through_the_min_branch() {
    let odd = ["a", "   b", "    c"];
    assert_eq!(relative_indent_depths(&odd), vec![0, 1, 1]);
}

#[test]
fn indent_unit_uses_the_gap_from_the_minimum_not_the_sum() {
    // indents [1, 2, 4]: min 1, original steps [0, 1, 3], unit 1, depths [0, 1, 3].
    // Adding the minimum instead yields steps [2, 3, 5], unit 2, depths [0, 1, 2].
    let lines = [" a", "  b", "    c"];
    let depths = relative_indent_depths(&lines);
    assert_eq!(depths, vec![0, 1, 3], "{depths:?}");
}

#[test]
fn depth_rounding_divides_by_the_unit_not_taking_remainder() {
    // indents [0, 3] with unit 3: (3 - 0 + 1) / 3 = 1. Remainder 3 % 3 = 0 would collapse
    // the deeper line to the same depth as the shallow one.
    let lines = ["a", "   b"];
    let depths = relative_indent_depths(&lines);
    assert_eq!(depths, vec![0, 1], "{depths:?}");
}

#[test]
fn tab_indented_actual_keeps_tabs_when_the_anchor_was_spaces() {
    let old = "    fn a() {}\n    fn b() {}\n";
    let actual = "\tfn a() {}\n\tfn b() {}\n";
    let new = "    fn a() {}\n    fn c() {}\n";
    let out = adjust_indentation(old, actual, new);
    assert!(
        out.contains('\t'),
        "the file's tab indent is preserved: {out:?}"
    );
    assert!(
        !out.contains("    fn"),
        "spaces from the model must not win over tabs in the file: {out:?}"
    );
}
