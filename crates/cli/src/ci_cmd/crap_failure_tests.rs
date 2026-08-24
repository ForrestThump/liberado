//! Split from `ci_cmd.rs` for module health.

use super::crap_failure::emit_crap_failure_to;
use super::{CRAP_CEILING, CRAP_CEILING_HINT};

/// The four annotation branches stay covered without any test touching process env.
#[test]
fn crap_failure_annotation_branches_cover_both_gates() {
    for (on_ci, has_ratchet) in [(true, true), (true, false), (false, true), (false, false)] {
        let error =
            emit_crap_failure_to(on_ci, has_ratchet, "cargo crap failed".into()).to_string();
        assert!(error.contains("cargo crap failed"), "{error}");
        if has_ratchet {
            assert!(error.contains("Do not raise the baseline"), "{error}");
        } else {
            assert!(error.contains(CRAP_CEILING), "{error}");
        }
    }
}

/// Pin that the production entry point still reads the env var itself.
#[test]
fn emit_crap_failure_wraps_the_inner_emitter() {
    let error = super::emit_crap_failure(false, "gate".into()).to_string();
    assert!(error.contains("gate"), "{error}");
    assert!(error.contains(CRAP_CEILING_HINT.trim()), "{error}");
}
