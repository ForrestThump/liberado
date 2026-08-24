//! Split from `coding_goal.rs`: kills the baseline campaign's survivors.
//!
//! Pins every payload accessor the pack reads: interactive, fanout_child,
//! write_scope.

use super::*;

fn payload_from(json: serde_json::Value) -> CodingGoalPayload {
    CodingGoalPayload::parse(&json).expect("payload json")
}

#[test]
fn interactive_is_absent_by_default() {
    assert_eq!(payload_from(serde_json::json!({})).interactive(), None);
}

#[test]
fn interactive_reads_the_declared_value() {
    assert_eq!(
        payload_from(serde_json::json!({ "interactive": true })).interactive(),
        Some(true)
    );
    assert_eq!(
        payload_from(serde_json::json!({ "interactive": false })).interactive(),
        Some(false)
    );
}

#[test]
fn fanout_child_defaults_to_false_and_reads_true() {
    assert!(!payload_from(serde_json::json!({})).fanout_child());
    assert!(payload_from(serde_json::json!({ "fanout_child": true })).fanout_child());
}

#[test]
fn write_scope_is_absent_by_default_and_present_when_set() {
    let bare = payload_from(serde_json::json!({}));
    assert!(bare.write_scope().is_none(), "{bare:?}");

    let scoped = payload_from(serde_json::json!({
        "write_scope": { "deny_globs": ["secrets/**"] }
    }));
    let scope = scoped.write_scope().expect("scope present");
    assert_eq!(scope.deny_globs, vec!["secrets/**".to_string()]);
}
