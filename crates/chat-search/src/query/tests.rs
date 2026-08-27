//! Survivor tests for `ParsedQuery::find_start` — the snippet centering offset.

use super::*;

/// `find_start` returns the byte offset of the FIRST match, and `None` when there is none.
/// Both constant-`Some` mutants survived every "matches() is true" assertion; only offsets pin it.
#[test]
fn find_start_reports_the_first_match_offset_or_none() {
    let q = ParsedQuery::parse_literal("needle").unwrap();
    let hay = "lots of text before the needle appears; needle again later";
    assert_eq!(q.find_start(hay), Some(24), "first occurrence wins");
    assert_eq!(q.find_start("nothing here"), None);

    let re = ParsedQuery::parse_regex(r"\d+").unwrap();
    assert_eq!(re.find_start("ab 123 cd"), Some(3));
    assert_eq!(re.find_start("no digits"), None);
}
