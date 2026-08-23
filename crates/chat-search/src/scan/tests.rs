//! Survivor tests for snippet ellipsis bounds and the missing-root error contract.

use super::*;
use crate::query::ParsedQuery;

#[test]
fn snippet_adds_ellipsis_only_on_truncated_sides() {
    let q = ParsedQuery::parse_literal("target").unwrap();

    // Match in the middle of a long document: both sides truncated → both ellipses.
    let filler = "x".repeat(400);
    let content = format!("{filler} target {filler}");
    let s = snippet(&content, &q);
    assert!(
        s.starts_with('\u{2026}'),
        "leading truncation gets an ellipsis: {s:?}"
    );
    assert!(
        s.ends_with('\u{2026}'),
        "trailing truncation gets an ellipsis: {s:?}"
    );

    // Match at the very start of a short message: neither side truncated → no ellipses.
    let s = snippet("target found", &q);
    assert_eq!(s, "target found", "no ellipsis when nothing was cut");

    // Match at the very start of a long tail: leading cut, no trailing cut.
    let content = format!("{} target {}", "y".repeat(400), "z".repeat(30));
    let s = snippet(&content, &q);
    assert!(s.starts_with('\u{2026}'));
    assert!(!s.ends_with('\u{2026}'), "tail fully included: {s:?}");
}

#[tokio::test]
async fn a_missing_root_is_empty_but_a_broken_root_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("conv.jsonl"),
        r#"{"author":"User","content":"hello target world"}"#,
    )
    .unwrap();

    // Missing directory: an honest empty result.
    let missing = dir.path().join("does-not-exist");
    let r = search(&missing, &ParsedQuery::parse_literal("target").unwrap(), 10)
        .await
        .unwrap();
    assert!(r.matches.is_empty());
    assert_eq!(r.total_found, 0);

    // A root that exists but cannot be read as a directory (a plain file) must surface as an
    // I/O error — a swallowed error would silently report "no matches" forever.
    let file_root = dir.path().join("root-is-a-file");
    std::fs::write(&file_root, b"not a directory").unwrap();
    let err = search(
        &file_root,
        &ParsedQuery::parse_literal("target").unwrap(),
        10,
    )
    .await;
    assert!(
        err.is_err(),
        "non-NotFound read_dir failures must propagate"
    );
}
