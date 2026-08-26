//! Survivor tests for `hashline.rs`.
//!
//! Wired as a sibling module so the private pipeline — locator parsing, edit
//! partitioning, bounds validation, bottom-up application — stays directly
//! assertable. Text-level assertions pin exact output strings; error-path
//! assertions pin exact message fragments.

use super::*;
use std::collections::BTreeMap;

// ── normalization and hashing ───────────────────────────────────────────────

/// BOM gone, CRLF flattened, trailing spaces/tabs trimmed per line, interior
/// whitespace preserved, structure joined back with LF.
#[test]
fn normalize_for_hash_output_is_exact() {
    let input = "\u{FEFF}a \r\n\tb\t\r\n";
    assert_eq!(normalize_for_hash(input), "a\n\tb\n");
    assert_eq!(normalize_for_hash("plain"), "plain");
    assert_eq!(normalize_for_hash(""), "");
}

/// The tag is clamped to the configured length window, and every digit after
/// the first can be non-zero: the radix loop must keep consuming the whole
/// value instead of collapsing it on the first step.
#[test]
fn compute_file_hash_clamps_length_and_uses_all_digits() {
    assert_eq!(
        compute_file_hash("hello\n", 3),
        compute_file_hash("hello\n", 4)
    );
    assert_eq!(
        compute_file_hash("hello\n", 99),
        compute_file_hash("hello\n", 10)
    );

    // Independently verified: the true tag of this input has X in second place.
    let tag = compute_file_hash("hello\n", 10);
    assert_eq!(tag, "AXJ5131RRM");
    assert_ne!(tag.chars().nth(1), Some('0'));
}

// ── error previews ──────────────────────────────────────────────────────────

/// Error messages quote the offending text as a JSON string.
#[test]
fn error_previews_are_json_quoted() {
    let err = parse_header("no brackets here").unwrap_err();
    assert!(
        err.contains("\"no brackets here\""),
        "preview must be JSON-quoted: {err}"
    );

    let err = parse_patch("[a#b#c#TAG1]\nPUT <1:\n+x").unwrap_err();
    assert!(err.contains("must not contain '#'"), "{err}");
    assert!(err.contains("\"a#b#c\""), "{err}");
}

// ── header parsing ──────────────────────────────────────────────────────────

/// A body line without brackets is a header-syntax error, not silently
/// swallowed content.
#[test]
fn bracketless_lines_are_rejected_as_headers() {
    let err = parse_patch("plainline\n").unwrap_err();
    assert!(
        err.contains("must start with [PATH#TAG]"),
        "unexpected error: {err}"
    );
}

/// A line with only one bracket is equally rejected: both delimiters are
/// required before any slicing happens.
#[test]
fn half_bracketed_lines_are_rejected_as_headers() {
    let err = parse_patch("[f.txt\nPUT <1:\n+A").unwrap_err();
    assert!(
        err.contains("must start with [PATH#TAG]"),
        "unexpected error: {err}"
    );
}

// ── read-prefix stripping ───────────────────────────────────────────────────

/// Bare pasted rows lose an `N:` read-output prefix; everything else passes
/// through untouched.
#[test]
fn strip_read_prefix_case_matrix() {
    assert_eq!(strip_read_prefix("12: hello"), " hello");
    assert_eq!(strip_read_prefix("0000:"), "", "empty remainder is fine");
    assert_eq!(strip_read_prefix("12ab"), "12ab", "colon required");
    assert_eq!(strip_read_prefix("12"), "12", "digits to end-of-line stay");
    assert_eq!(
        strip_read_prefix(":x"),
        ":x",
        "nothing digit-led, nothing stripped"
    );
    assert_eq!(strip_read_prefix("a1:b"), "a1:b", "must start with digits");
    assert_eq!(
        strip_read_prefix("1 : x"),
        "1 : x",
        "space breaks the prefix"
    );
    assert_eq!(strip_read_prefix(""), "");
}

// ── op headers inside PUT bodies ────────────────────────────────────────────

/// An `MV` line terminates a PUT body and surfaces the unsupported-op error
/// rather than being swallowed as payload.
#[test]
fn mv_line_terminates_put_body_and_errors() {
    let patch = "[f.txt#TAG1]\nPUT <1:\n+A\nMV elsewhere\n+B";
    let err = parse_patch(patch).unwrap_err();
    assert!(err.contains("MV (move/rename) is not supported"), "{err}");
}

/// A following section header terminates a PUT body: the patch splits into
/// two sections rather than swallowing the header as payload.
#[test]
fn next_section_header_terminates_put_body() {
    let patch = "[f.txt#TAG1]\nPUT <1:\n+A\n[g.txt#TAG2]\nPUT <1:\n+B";
    let sections = parse_patch(patch).unwrap();
    assert_eq!(sections.len(), 2, "{sections:?}");
    assert_eq!(sections[0].path, "f.txt");
    assert_eq!(sections[1].path, "g.txt");
    assert_eq!(
        sections[0].edits,
        vec![Edit::InsertBof { text: "A".into() }]
    );
}

// ── blank-row layout rules inside PUT bodies ────────────────────────────────

fn bof_edits(body: &str) -> Vec<Edit> {
    let patch = format!("[f.txt#TAG1]\nPUT <1:\n{body}");
    parse_patch(&patch).unwrap().remove(0).edits
}

/// Interior blanks are content; runs of them collapse to one empty payload;
/// leading blanks are layout; trailing blanks before end-of-input vanish.
#[test]
fn put_body_blank_rows_follow_layout_rules() {
    // Leading blank is layout.
    let edits = bof_edits("\n+A\n");
    assert_eq!(
        edits,
        vec![Edit::InsertBof { text: "A".into() }],
        "leading blank dropped: {edits:?}"
    );

    // Each interior blank line contributes exactly one empty payload row,
    // decided by peeking past the whole blank run.
    let edits = bof_edits("+A\n\n\n+B\n");
    assert_eq!(
        edits,
        vec![
            Edit::InsertBof { text: "A".into() },
            Edit::InsertBof {
                text: String::new()
            },
            Edit::InsertBof {
                text: String::new()
            },
            Edit::InsertBof { text: "B".into() },
        ],
        "two interior blanks, two empty rows: {edits:?}"
    );

    // Trailing blanks before end-of-input are layout only.
    let edits = bof_edits("+A\n\n\n");
    assert_eq!(
        edits,
        vec![Edit::InsertBof { text: "A".into() }],
        "trailing blanks dropped: {edits:?}"
    );
}

/// Blanks followed by a real op header end the body instead of being eaten as
/// payload rows.
#[test]
fn blanks_before_an_op_header_end_the_put_body() {
    let patch = "[f.txt#TAG1]\nPUT <1:\n+A\n\n\nCUT 1";
    let sections = parse_patch(patch).unwrap();
    assert_eq!(
        sections[0].edits,
        vec![
            Edit::InsertBof { text: "A".into() },
            Edit::Delete { line: 1 },
        ],
        "blank run stays layout, CUT still parses: {:?}",
        sections[0].edits
    );
}

// ── comment and blank skipping in section bodies ────────────────────────────

#[test]
fn comment_lines_are_skipped_between_ops() {
    let patch = "[f.txt#TAG1]\n# lead\nCUT 1.=1\n# trail";
    let sections = parse_patch(patch).unwrap();
    assert_eq!(sections[0].edits.len(), 1, "{:?}", sections[0].edits);
    assert_eq!(sections[0].edits[0], Edit::Delete { line: 1 });
}

// ── bounds validation ───────────────────────────────────────────────────────

fn anchored(line: usize) -> BTreeMap<usize, Vec<&'static Edit>> {
    let leaked: &'static Edit = Box::leak(Box::new(Edit::Delete { line }));
    let mut map = BTreeMap::new();
    map.insert(line, vec![leaked]);
    map
}

fn bounds_of_at(text: &str, line: usize) -> Result<(), String> {
    let lines = prepare_lines(text);
    validate_bounds(text, &lines, &anchored(line))
}

/// Anchor lines must name real lines: exact messages distinguish an empty
/// file from an out-of-range anchor, phantom-newline files count their real
/// lines only, and the last line of a newline-terminated file is valid.
#[test]
fn validate_bounds_messages_and_boundaries() {
    // Empty file: every anchor is rejected with the empty-file message,
    // however far out of range it sits.
    assert_eq!(
        bounds_of_at("", 1).unwrap_err(),
        "line 1 does not exist (file is empty)"
    );
    assert_eq!(
        bounds_of_at("", 2).unwrap_err(),
        "line 2 does not exist (file is empty)"
    );

    // Trailing newline creates a phantom that is not an addressable line.
    assert_eq!(
        bounds_of_at("a\n", 2).unwrap_err(),
        "line 2 does not exist (file has 1 lines)"
    );

    // Without the trailing newline both lines exist; the third does not.
    assert!(bounds_of_at("a\nb", 2).is_ok());
    assert_eq!(
        bounds_of_at("a\nb", 3).unwrap_err(),
        "line 3 does not exist (file has 2 lines)"
    );
}

// ── application pipeline ────────────────────────────────────────────────────

/// BOF inserts into a genuinely empty file replace the phantom; into any
/// non-empty file they land at the front in payload order.
#[test]
fn apply_bof_empty_vs_nonempty_targets() {
    let bof = vec!["X".to_string(), "Y".to_string()];

    let mut empty_lines = vec![String::new()];
    let mut first = None;
    apply_bof(&mut empty_lines, bof.clone(), &mut first);
    assert_eq!(empty_lines, vec!["X", "Y"]);
    assert_eq!(first, Some(1));

    let mut lines = vec!["a".to_string(), "b".to_string()];
    let mut first = None;
    apply_bof(&mut lines, bof.clone(), &mut first);
    assert_eq!(lines, vec!["X", "Y", "a", "b"]);

    // A one-line file that is not empty must be inserted into, not replaced.
    let mut single = vec!["only".to_string()];
    let mut first = None;
    apply_bof(&mut single, bof, &mut first);
    assert_eq!(single, vec!["X", "Y", "only"]);
}

/// EOF inserts respect the trailing-newline phantom, land at the true end
/// otherwise, replace the empty-file phantom, preserve payload order, and
/// report the first touched line.
#[test]
fn apply_eof_positions_and_first_changed() {
    // With trailing newline: insert before the phantom; final newline survives.
    let text = "a\nb\n";
    let mut lines = prepare_lines(text);
    let mut first = None;
    apply_eof(text, &mut lines, vec!["X".into()], &mut first);
    assert_eq!(lines.join("\n"), "a\nb\nX\n");
    assert_eq!(first, Some(3));

    // Two payloads keep their order.
    let mut lines = prepare_lines(text);
    let mut first = None;
    apply_eof(text, &mut lines, vec!["X".into(), "Y".into()], &mut first);
    assert_eq!(lines.join("\n"), "a\nb\nX\nY\n");

    // No trailing newline: append at the physical end.
    let text = "a\nb";
    let mut lines = prepare_lines(text);
    let mut first = None;
    apply_eof(text, &mut lines, vec!["X".into()], &mut first);
    assert_eq!(lines.join("\n"), "a\nb\nX");

    // Empty file: phantom replaced outright.
    let text = "";
    let mut lines = prepare_lines(text);
    let mut first = None;
    apply_eof(text, &mut lines, vec!["X".into(), "Y".into()], &mut first);
    assert_eq!(lines, vec!["X", "Y"]);
}

/// End-to-end: a BOF-plus-EOF patch against an empty file yields exactly the
/// two inserted lines and nothing else.
#[test]
fn empty_file_bof_and_eof_end_to_end() {
    let patch = "[new.txt#TAG1]\nPUT <1:\n+HEAD\nPUT >$:\n+TAIL";
    let sections = parse_patch(patch).unwrap();
    let (after, first) = apply_edits("", &sections[0].edits).unwrap();
    assert_eq!(after, "HEAD\nTAIL");
    assert_eq!(first, Some(1));
}

#[test]
fn strip_keyword_matches_the_bare_keyword() {
    // `s.len() == keyword.len()` is a hit: empty rest is allowed. `<=` would
    // reject the bare keyword before the equality check runs.
    assert_eq!(strip_keyword("CUT", "CUT"), Some(""));
    assert_eq!(strip_keyword("PUT", "PUT"), Some(""));
    assert_eq!(strip_keyword("CUT 1", "CUT"), Some(" 1"));
    assert!(strip_keyword("CU", "CUT").is_none());
}

#[test]
fn put_body_line_ending_with_bracket_is_payload_not_a_header() {
    // The stop condition is `is_op_header || (starts_with('[') && ends_with(']'))`.
    // Turning the inner `&&` into `||` treats any line that merely ends with `]`
    // as the next section, so `+hello]` would never land in the PUT.
    let patch = "[f.txt#TAG1]\nPUT <1:\n+hello]\n";
    let sections = parse_patch(patch).expect("a payload that ends with ] is still payload");
    assert_eq!(
        sections[0].edits,
        vec![Edit::InsertBof {
            text: "hello]".into()
        }],
        "{sections:?}"
    );
}
