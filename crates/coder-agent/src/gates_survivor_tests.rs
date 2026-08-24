//! Split from `gates.rs`: kills the baseline campaign's survivors.
//!
//! Pins the minimum-length boundary in git status parsing.

use super::*;

#[test]
fn a_shortest_well_formed_status_line_parses() {
    // Exactly four bytes: code column plus one path character. The length gate
    // is `< 4`; an off-by-one in either direction loses this path.
    assert_eq!(parse_status_path("M  f"), Some("f".to_string()));
}

#[test]
fn short_lines_yield_no_path() {
    assert_eq!(parse_status_path(""), None);
    assert_eq!(parse_status_path("M "), None);
    assert_eq!(parse_status_path("MM  "), None, "empty path after the code");
}
