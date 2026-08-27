//! Split from `goals.rs` for module-health boundaries.

use super::*;

#[test]
fn windows_extended_prefixes_are_stripped_for_the_wire() {
    use std::path::Path;
    assert_eq!(
        strip_windows_extended_path(Path::new(r"\\?\C:\repos\liberado")),
        r"C:\repos\liberado"
    );
    assert_eq!(
        strip_windows_extended_path(Path::new(r"\\?\UNC\server\share\repo")),
        r"\\server\share\repo"
    );
    assert_eq!(
        strip_windows_extended_path(Path::new("/home/user/repo")),
        "/home/user/repo",
        "plain paths pass through untouched"
    );
}

/// A diff between the old 2 KiB floor and the real cap must round-trip: a cap that
/// silently shrank would truncate readable diffs while every huge-fixture test still
/// passed.
#[test]
fn a_mid_sized_diff_is_not_truncated() {
    // An independent size, not derived from the cap: a cap that silently shrank to a few
    // kilobytes must show up here, not hide behind the shared constant.
    let mid = "x".repeat(3_000);
    assert_eq!(bound_diff(mid.clone()), mid);
}

/// The cut lands on a char boundary and keeps an exact, non-empty prefix of the input.
#[test]
fn truncation_lands_on_a_char_boundary_keeping_exact_content() {
    // Byte MAX_DIFF_BYTES falls inside the first three-byte character.
    let mut diff = "a".repeat(MAX_DIFF_BYTES - 1);
    diff.push_str("漢漢漢");
    diff.push_str(&"b".repeat(1_000));

    let bounded = bound_diff(diff.clone());
    assert!(bounded.contains("[diff truncated"), "{bounded}");
    let head = bounded.split("\n\n[").next().unwrap();
    assert!(!head.is_empty(), "some content is kept");
    assert!(
        head.len() <= MAX_DIFF_BYTES,
        "the kept prefix stays within the cap: {}",
        head.len()
    );
    assert!(
        diff.starts_with(head),
        "kept bytes are an exact prefix of the input"
    );
}
