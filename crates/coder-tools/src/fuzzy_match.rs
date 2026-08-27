//! Finding an anchor in a file when the model's copy is *nearly* right.
//!
//! ## Why
//!
//! Exact string matching fails on differences that carry no meaning. A dispatched run produced
//! this anchor for `crates/executor/src/lib.rs`:
//!
//! ```text
//!     /// Receives a [`TurnRecord`] per completed turn.
//! ```
//!
//! Character-for-character correct except for four leading spaces the file does not have. The
//! edit was rejected, the model re-read, guessed again, and the run died after 21 such failures.
//! Across four dispatched runs the anchor failure rate never dropped below 42%.
//!
//! ## Where this comes from
//!
//! Ported from `oh-my-pi`'s `packages/coding-agent/src/edit/modes/replace.ts` and
//! `edit/normalize.ts`, which ship `edit.fuzzyMatch: true` at `fuzzyThreshold: 0.95` by default.
//! Their thresholds and their acceptance rule are reproduced rather than re-derived: they are
//! tuned against far more traffic than we have, and inventing our own numbers would be guessing
//! dressed as engineering.
//!
//! ## The algorithm
//!
//! 1. **Exact match first**, always. Fuzzy never runs when an exact anchor exists, so this cannot
//!    change the behaviour of an edit that already worked.
//! 2. Otherwise slide a window the height of the target over the file. Normalize both sides per
//!    line — trim, collapse runs of whitespace, fold smart quotes and dashes — and prefix each
//!    line with its *relative* indent depth. Absolute indentation stops mattering; structure
//!    still does, so a body and its signature cannot swap places.
//! 3. Score each window as the mean per-line similarity, `1 - levenshtein / max_len`.
//! 4. Accept the best window when it clears the threshold **and** is unambiguous: either it is
//!    the only window above the line, or it is *dominant* — at least 0.97 and clear of the
//!    runner-up by 0.08.
//!
//! Rule 4 is the safety property. A fuzzy matcher that silently picks between two plausible
//! sites will eventually edit the wrong one, and an edit in the wrong place is far worse than a
//! rejected edit: the run reports success and the mistake ships.

/// oh-my-pi's default, owned by `EditConfig` so the config and the matcher cannot drift apart.
///
/// A line differing only in indentation scores near 1.0 once normalized; a line with a genuinely
/// different identifier does not.
/// Production reads the threshold from `EditConfig`, so this is used only by this module's
/// tests — kept because a test that hardcodes 0.95 stops testing the default the moment the
/// default moves.
#[cfg(test)]
pub const DEFAULT_THRESHOLD: f64 = liberado_coder_core::EditConfig::DEFAULT_FUZZY_THRESHOLD;

/// Confidence a match needs before it may win *despite* other candidates clearing the threshold.
const DOMINANT_MIN_CONFIDENCE: f64 = 0.97;

/// How far a dominant match must beat the runner-up.
const DOMINANT_DELTA: f64 = 0.08;

/// A window of the file that resembles the target.
#[derive(Debug, Clone, PartialEq)]
pub struct FuzzyMatch {
    /// The file's own text for this window — what must actually be replaced.
    pub actual_text: String,
    /// Byte offset of the window's first character.
    pub start_index: usize,
    /// 1-based line number, for error messages a human can act on.
    pub start_line: usize,
    pub confidence: f64,
}

/// What a search found.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchOutcome {
    /// The target appears verbatim. `count` is how many times.
    Exact { index: usize, count: usize },
    /// One window is close enough and unambiguous.
    Fuzzy(FuzzyMatch),
    /// Several windows are close enough. Deliberately not resolved — see the module docs.
    Ambiguous { count: usize, best: FuzzyMatch },
    /// Nothing cleared the bar. `closest` is carried so the error can quote what was nearly it.
    NotFound { closest: Option<FuzzyMatch> },
}

/// Edit distance. Two rolling rows rather than a full matrix: an anchor is at most a few hundred
/// characters and this runs once per window per edit.
pub fn levenshtein(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// 0.0 to 1.0, where 1.0 is identical.
pub fn similarity(a: &str, b: &str) -> f64 {
    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (levenshtein(a, b) as f64 / max_len as f64)
}

/// Fold away differences that carry no meaning: surrounding space, runs of whitespace, and the
/// typographic characters an editor or a model substitutes without being asked.
pub fn normalize_for_fuzzy(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(trimmed.len());
    let mut last_was_space = false;
    for ch in trimmed.chars() {
        let mapped = match ch {
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' | '\u{00ab}' | '\u{00bb}' => '"',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '`' | '\u{00b4}' => '\'',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            ' ' | '\t' => ' ',
            other => other,
        };
        if mapped == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        out.push(mapped);
    }
    out
}

fn leading_whitespace(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// Indent depth of each line *relative to the block's own shallowest line*, in units of the
/// block's own smallest indent step.
///
/// This is what lets an anchor match when the whole block sits at a different indentation than
/// the model wrote, while still refusing to match a block whose internal shape differs.
fn relative_indent_depths(lines: &[&str]) -> Vec<usize> {
    let indents: Vec<usize> = lines.iter().map(|l| leading_whitespace(l)).collect();
    let non_empty: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, _)| indents[i])
        .collect();
    let min_indent = non_empty.iter().copied().min().unwrap_or(0);
    let unit = non_empty
        .iter()
        .map(|i| i - min_indent)
        .filter(|s| *s > 0)
        .min()
        .unwrap_or(1);
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if l.trim().is_empty() || unit == 0 {
                0
            } else {
                (indents[i] - min_indent).div_ceil(unit).min(
                    // round-to-nearest, matching the reference's Math.round
                    (indents[i] - min_indent + unit / 2) / unit,
                )
            }
        })
        .collect()
}

fn normalize_lines(lines: &[&str]) -> Vec<String> {
    let depths = relative_indent_depths(lines);
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{}|{}", depths[i], normalize_for_fuzzy(line)))
        .collect()
}

/// Byte offset of the start of each line.
fn line_offsets(content: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut at = 0usize;
    for line in content.split('\n') {
        offsets.push(at);
        at += line.len() + 1;
    }
    offsets
}

/// Find `target` in `content`, exactly if possible and approximately if allowed.
pub fn find_match(content: &str, target: &str, allow_fuzzy: bool, threshold: f64) -> MatchOutcome {
    if target.is_empty() {
        return MatchOutcome::NotFound { closest: None };
    }

    let exact = content.matches(target).count();
    if exact > 0 {
        return MatchOutcome::Exact {
            index: content.find(target).unwrap_or(0),
            count: exact,
        };
    }

    let content_lines: Vec<&str> = content.split('\n').collect();
    let target_lines: Vec<&str> = target.split('\n').collect();
    if target_lines.len() > content_lines.len() {
        return MatchOutcome::NotFound { closest: None };
    }
    let offsets = line_offsets(content);
    let target_norm = normalize_lines(&target_lines);

    let mut best: Option<FuzzyMatch> = None;
    let mut best_score = -1.0f64;
    let mut second_best = -1.0f64;
    let mut above_threshold = 0usize;

    for start in 0..=(content_lines.len() - target_lines.len()) {
        let window = &content_lines[start..start + target_lines.len()];
        let window_norm = normalize_lines(window);
        let score: f64 = target_norm
            .iter()
            .zip(window_norm.iter())
            .map(|(t, w)| similarity(t, w))
            .sum::<f64>()
            / target_lines.len() as f64;

        if score >= threshold {
            above_threshold += 1;
        }
        if score > best_score {
            second_best = best_score;
            best_score = score;
            best = Some(FuzzyMatch {
                actual_text: window.join("\n"),
                start_index: offsets[start],
                start_line: start + 1,
                confidence: score,
            });
        } else if score > second_best {
            second_best = score;
        }
    }

    let Some(best) = best else {
        return MatchOutcome::NotFound { closest: None };
    };

    if allow_fuzzy && best.confidence >= threshold {
        if above_threshold == 1 {
            return MatchOutcome::Fuzzy(best);
        }
        if best.confidence >= DOMINANT_MIN_CONFIDENCE
            && best.confidence - second_best >= DOMINANT_DELTA
        {
            return MatchOutcome::Fuzzy(best);
        }
        return MatchOutcome::Ambiguous {
            count: above_threshold,
            best,
        };
    }
    MatchOutcome::NotFound {
        closest: Some(best),
    }
}

/// Re-indent a replacement to sit where the match was actually found.
///
/// Fixing the anchor without this only moves the problem: the edit lands, and the inserted code
/// is indented to where the model *thought* the block was. The whole block shifts by the same
/// amount the anchor was wrong by.
///
/// **Only the uniform-shift case is ported.** oh-my-pi additionally handles tab/space conversion
/// and blocks with mixed indentation, which needs an indent-profile machine. Those fall through
/// to the replacement as written — the same result as not having this function, so the gap costs
/// nothing that was not already missing, and the uniform case is the one the observed failures
/// were made of.
pub fn adjust_indentation(old_text: &str, actual_text: &str, new_text: &str) -> String {
    if old_text == actual_text {
        return new_text.to_string();
    }
    // A patch that only re-indents must be applied exactly as written, or this would undo it.
    let same_content = old_text.lines().count() == new_text.lines().count()
        && old_text
            .lines()
            .zip(new_text.lines())
            .all(|(a, b)| a.trim() == b.trim());
    if same_content {
        return new_text.to_string();
    }

    let first_indent = |text: &str| -> Option<(usize, char)> {
        text.lines().find(|l| !l.trim().is_empty()).map(|l| {
            let n = leading_whitespace(l);
            let ch = l.chars().next().filter(|c| *c == '\t').unwrap_or(' ');
            (n, ch)
        })
    };
    let (Some((old_indent, _)), Some((actual_indent, indent_char))) =
        (first_indent(old_text), first_indent(actual_text))
    else {
        return new_text.to_string();
    };
    if old_indent == actual_indent {
        return new_text.to_string();
    }

    let delta = actual_indent as isize - old_indent as isize;
    new_text
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                return line.to_string();
            }
            let current = leading_whitespace(line);
            let target = (current as isize + delta).max(0) as usize;
            format!(
                "{}{}",
                std::iter::repeat_n(indent_char, target).collect::<String>(),
                line.trim_start()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if new_text.ends_with('\n') { "\n" } else { "" }
}

#[cfg(test)]
#[path = "fuzzy_match_survivor_tests.rs"]
mod survivor_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure this module exists for, taken verbatim from a dispatched run: the anchor was
    /// right except for four leading spaces the file does not have.
    #[test]
    fn an_anchor_with_the_wrong_indentation_is_found() {
        let content =
            "mod x {\n}\n/// Receives a [`TurnRecord`] per completed turn.\npub trait T {}\n";
        let target = "    /// Receives a [`TurnRecord`] per completed turn.";
        match find_match(content, target, true, DEFAULT_THRESHOLD) {
            MatchOutcome::Fuzzy(m) => {
                assert_eq!(
                    m.actual_text,
                    "/// Receives a [`TurnRecord`] per completed turn."
                );
                assert!(m.confidence >= DEFAULT_THRESHOLD, "{}", m.confidence);
            }
            other => panic!("expected a fuzzy match, got {other:?}"),
        }
    }

    /// Exact matching must be untouched. Fuzzy that changed the behaviour of an edit which
    /// already worked would be a regression dressed as a feature.
    #[test]
    fn an_exact_match_never_reaches_the_fuzzy_path() {
        let content = "alpha\nbeta\ngamma\n";
        assert_eq!(
            find_match(content, "beta", true, DEFAULT_THRESHOLD),
            MatchOutcome::Exact { index: 6, count: 1 }
        );
    }

    #[test]
    fn a_repeated_exact_match_is_still_counted() {
        match find_match("a\nx\na\n", "a", true, DEFAULT_THRESHOLD) {
            MatchOutcome::Exact { count, .. } => assert_eq!(count, 2),
            other => panic!("expected exact, got {other:?}"),
        }
    }

    /// The safety property. Two equally plausible sites must be reported, never chosen between —
    /// an edit in the wrong place reports success and ships.
    #[test]
    fn two_equally_good_candidates_are_refused_not_guessed() {
        // Tabs in the file, spaces in the anchor: no substring relationship, so this reaches the
        // fuzzy path rather than being caught as a repeated exact match. Both sites normalize
        // identically, so neither can dominate.
        let content = "fn a() {\n\tdo_work();\n}\n\nfn b() {\n\tdo_work();\n}\n";
        match find_match(content, "  do_work();", true, DEFAULT_THRESHOLD) {
            MatchOutcome::Ambiguous { count, .. } => assert!(count >= 2, "count {count}"),
            other => panic!("ambiguity must not be resolved silently, got {other:?}"),
        }
    }

    /// An anchor that is a *substring* of a longer line is still a repeated exact match, and the
    /// caller must see the count so it can refuse. Fuzzy never gets a say.
    #[test]
    fn a_substring_anchor_is_reported_as_a_repeated_exact_match() {
        let content = "fn a() {\n    do_work();\n}\nfn b() {\n    do_work();\n}\n";
        match find_match(content, "  do_work();", true, DEFAULT_THRESHOLD) {
            MatchOutcome::Exact { count, .. } => assert_eq!(count, 2),
            other => panic!("expected a repeated exact match, got {other:?}"),
        }
    }

    /// A clearly-better candidate may win even when a weaker one also clears the bar, which is
    /// what stops the ambiguity rule from rejecting every edit in a repetitive file.
    #[test]
    fn a_dominant_candidate_wins_over_a_weaker_one() {
        let content = "let alpha = compute(1);\nlet alphabet_total = compute(22222);\n";
        match find_match(content, "let alpha = compute(1)", true, DEFAULT_THRESHOLD) {
            MatchOutcome::Exact { .. } | MatchOutcome::Fuzzy(_) => {}
            other => panic!("a clear winner must be usable, got {other:?}"),
        }
    }

    /// Fuzzy off must behave exactly as before: no match, but the near-miss is carried so the
    /// error can quote it.
    #[test]
    fn with_fuzzy_disabled_a_near_miss_is_reported_not_used() {
        let content = "/// a doc line\n";
        match find_match(content, "    /// a doc line", false, DEFAULT_THRESHOLD) {
            MatchOutcome::NotFound { closest: Some(c) } => {
                assert!(c.confidence > 0.9, "{}", c.confidence)
            }
            other => panic!("expected a reported near-miss, got {other:?}"),
        }
    }

    /// Structure still has to match. Relative indent depth is in the normalized line precisely so
    /// that a body cannot match its own signature.
    #[test]
    fn a_structurally_different_block_is_not_a_match() {
        let content = "fn outer() {\n    fn inner() {\n        body();\n    }\n}\n";
        let target = "fn wildly_different_name(a: u32, b: u32) -> String {\n    unrelated();\n}";
        assert!(
            matches!(
                find_match(content, target, true, DEFAULT_THRESHOLD),
                MatchOutcome::NotFound { .. }
            ),
            "unrelated code must not be edited"
        );
    }

    #[test]
    fn smart_quotes_and_dashes_are_folded() {
        assert_eq!(
            normalize_for_fuzzy("\u{201c}hi\u{201d} \u{2014} ok"),
            "\"hi\" - ok"
        );
        assert_eq!(normalize_for_fuzzy("a    b\t\tc"), "a b c");
    }

    #[test]
    fn similarity_is_one_for_identical_and_zero_for_disjoint() {
        assert_eq!(similarity("abc", "abc"), 1.0);
        assert!(similarity("abc", "xyz") < 0.01);
        assert_eq!(similarity("", ""), 1.0);
    }

    /// Fixing the anchor without re-indenting only moves the bug: the edit lands and the whole
    /// inserted block sits at the indentation the model imagined.
    #[test]
    fn a_replacement_is_re_indented_to_where_the_match_was_found() {
        let old = "    fn a() {}";
        let actual = "fn a() {}";
        let new = "    fn a() {}\n    fn b() {}";
        assert_eq!(adjust_indentation(old, actual, new), "fn a() {}\nfn b() {}");
    }

    /// An edit whose whole purpose is to change indentation must be applied verbatim.
    #[test]
    fn a_pure_reindentation_is_applied_as_written() {
        let old = "fn a() {}";
        let actual = "  fn a() {}";
        let new = "        fn a() {}";
        assert_eq!(adjust_indentation(old, actual, new), new);
    }

    #[test]
    fn an_exactly_matching_anchor_leaves_the_replacement_alone() {
        assert_eq!(adjust_indentation("x", "x", "    y"), "    y");
    }
}
