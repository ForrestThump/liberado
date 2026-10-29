//! Split from `ci_cmd.rs`: usage banner text.

use super::*;

#[test]
fn usage_names_the_diff_gate_verbs() {
    assert!(USAGE.contains("check"));
    assert!(USAGE.contains("crap"));
    assert!(USAGE.contains("complexity"));
    assert!(USAGE.contains("--base"));
    assert!(!USAGE.contains("complexity-ratchet"));
}
