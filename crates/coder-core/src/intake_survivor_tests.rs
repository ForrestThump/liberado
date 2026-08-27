//! Split from `intake.rs` for module-health boundaries.

//! Every visitor method of the two flexible deserializers, pinned through real JSON.
use super::*;
use serde_json::json;

#[test]
fn flexible_string_accepts_every_scalar_and_joins_sequences() {
    let q: IntakeQuestion = serde_json::from_value(json!({
        "id": 7,
        "prompt": ["line one", "line two", 3, null],
        "options": "single",
        "affects": true,
    }))
    .expect("scalars coerce");
    assert_eq!(q.id, "7");
    assert_eq!(q.prompt, "line one\nline two\n3");
    assert_eq!(q.options, vec!["single"]);
    assert_eq!(q.affects, "true");

    // Negative integers take the i64 arm; plain strings the str arm.
    let q: IntakeQuestion = serde_json::from_value(json!({
        "id": -5,
        "prompt": "plain text",
        "affects": "also plain",
    }))
    .unwrap();
    assert_eq!(q.id, "-5");
    assert_eq!(q.prompt, "plain text");
    assert_eq!(q.affects, "also plain");

    // f64 and explicit null paths.
    let q: IntakeQuestion = serde_json::from_value(json!({
        "id": 2.5,
        "prompt": null,
    }))
    .unwrap();
    assert_eq!(q.id, "2.5");
    assert_eq!(q.prompt, "");

    // Empty sequence joins to empty; nested values stringify compactly.
    let q: IntakeQuestion = serde_json::from_value(json!({
        "id": [],
        "prompt": [{"k": 1}],
        "options": [null, 4.5],
    }))
    .unwrap();
    assert_eq!(q.id, "");
    assert_eq!(q.prompt, "{\"k\":1}");
    assert_eq!(q.options, vec!["4.5"]);
}

#[test]
fn string_or_vec_accepts_string_list_map_null_and_coerces_members() {
    let d: GoalContractDraft = serde_json::from_value(json!({
        "description": "d",
        "success_criteria": "one",
        "out_of_scope": ["a", "b"],
        "assumed_defaults": {"k1": "v1", "k2": true},
    }))
    .unwrap();
    assert_eq!(d.success_criteria, vec!["one"]);
    assert_eq!(d.out_of_scope, vec!["a", "b"]);
    // Map form keeps values, drops keys.
    assert_eq!(d.assumed_defaults, vec!["v1", "true"]);

    let d: GoalContractDraft = serde_json::from_value(json!({
        "description": "d",
        "success_criteria": [42, -3, true, null, {"o": []}],
        "verifiers": [],
    }))
    .unwrap();
    assert_eq!(
        d.success_criteria,
        vec![
            "42".to_string(),
            "-3".to_string(),
            "true".to_string(),
            "{\"o\":[]}".to_string()
        ]
    );

    // Whitespace-only string is an EMPTY list, not a one-element list.
    let d: GoalContractDraft =
        serde_json::from_value(json!({"description": "d", "out_of_scope": "   "})).unwrap();
    assert!(d.out_of_scope.is_empty());
}

#[test]
fn wrong_container_types_fail_with_the_documented_expectation() {
    // An object where a flexible string is expected cannot be coerced — the error
    // text must name what was wanted, or triage loses its only hint.
    let err = serde_json::from_value::<IntakeQuestion>(json!({
        "id": {"nope": true},
        "prompt": "p",
    }))
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("a string, sequence of strings, or scalar"),
        "{err}"
    );

    let err = serde_json::from_value::<GoalContractDraft>(json!({
        "description": "d",
        "success_criteria": 1.5,
    }))
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("a string, sequence of strings, or empty"),
        "{err}"
    );
}
