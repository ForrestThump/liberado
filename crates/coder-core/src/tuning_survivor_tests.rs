//! Split from `tuning.rs` for module-health boundaries.

use super::*;

/// An empty `validation_command.program` is a config error, not a silent skip.
#[test]
fn empty_validation_command_program_is_rejected() {
    let tuning: CoderTuning = serde_json::from_value(serde_json::json!({
        "backend": "liberado-loop",
        "planner": {"model": "m", "prompt": "p", "max_turns": 3},
        "coder": {
            "model": "m",
            "prompt": "p",
            "max_turns": 3
        },
        "validation_command": {"program": "   ", "args": []},
        "critic": {"model": "m", "prompt": "p", "max_turns": 2},
    }))
    .expect("tuning fixture");
    let err = tuning.validate().unwrap_err().to_string();
    assert!(
        err.contains("validation_command.program must not be empty"),
        "{err}"
    );
}

/// Omitting `trace_formats` defaults to the native format only — a mutant that
/// empties the default silently turns off trace writing for every run.
#[test]
fn trace_formats_default_to_native() {
    let tuning: CoderTuning =
        serde_json::from_value(serde_json::json!({"backend": "liberado-loop"})).unwrap();
    assert_eq!(tuning.trace_formats, vec![TraceFormat::Native]);
}
