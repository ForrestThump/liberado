//! Split from `ci_cmd.rs`: usage banner text.

use super::*;

#[test]
fn usage_names_the_three_verbs() {
    assert!(USAGE.contains("check"));
    assert!(USAGE.contains("crap"));
    assert!(USAGE.contains("ratchet"));
}
