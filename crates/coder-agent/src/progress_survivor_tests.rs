//! Split from `progress.rs`: kills the baseline campaign's survivors.
//!
//! Covers the fatal naming/messages, the take-once latch contract, the exact
//! conditions that clear or keep a latched read-only stall, counter scoping for
//! unknown and multiplex tools, nudge-vs-fatal boundaries, and validation
//! signature/pass parsing.

use super::*;
use liberado_coder_core::ProgressPolicy;

fn policy() -> ProgressPolicy {
    ProgressPolicy {
        read_only_turn_limit: 2,
        same_tool_limit: 2,
        validation_repeat_limit: 2,
        max_attempts: 3,
        event_preview_max_chars: 100,
    }
}

fn isolated_policy() -> ProgressPolicy {
    ProgressPolicy {
        read_only_turn_limit: 100,
        same_tool_limit: 100,
        validation_repeat_limit: 2,
        max_attempts: 3,
        event_preview_max_chars: 100,
    }
}

fn latch_read_only_stall(guard: &mut ProgressGuard) {
    for t in ["list_files", "read_file", "git_status", "search_text"] {
        guard.observe(t, true, "{}");
    }
}

#[test]
fn each_fatal_variant_names_itself_and_its_counts() {
    let variants = [
        (
            ProgressFatal::ReadOnlyStall { consecutive: 6 },
            "read_only_stall",
            "6 consecutive inspect/tool calls",
        ),
        (
            ProgressFatal::ValidationChurn {
                signature: "sig-one".into(),
                repeats: 4,
            },
            "validation_churn",
            "validate failed 4 times",
        ),
        (
            ProgressFatal::SameToolChurn {
                tool: "search_text".into(),
                consecutive: 9,
            },
            "same_tool_churn",
            "`search_text` invoked 9 times",
        ),
    ];
    for (fatal, name, needle) in variants {
        assert_eq!(fatal.guard_name(), name);
        let msg = fatal.message();
        assert!(msg.contains("PROGRESS GUARD"), "{msg}");
        assert!(msg.contains(needle), "{msg}");
        assert!(
            msg.contains("submit_report"),
            "every fatal must point at the report escape hatch: {msg}"
        );
    }
}

#[test]
fn take_fatal_hands_out_the_latch_exactly_once() {
    let mut guard = ProgressGuard::new(policy());
    assert!(guard.take_fatal().is_none(), "nothing latched yet");
    latch_read_only_stall(&mut guard);
    let taken = guard.take_fatal();
    assert!(
        matches!(taken, Some(ProgressFatal::ReadOnlyStall { consecutive: 4 })),
        "{taken:?}"
    );
    assert!(guard.take_fatal().is_none(), "must be taken, not peeked");
}

/// A filed report is not workspace progress: it must leave a latched read-only
/// stall in place instead of silently clearing it.
#[test]
fn submitting_a_report_does_not_clear_the_stall() {
    let mut guard = ProgressGuard::new(policy());
    latch_read_only_stall(&mut guard);
    guard.observe("read_file", true, "{}");
    assert!(matches!(
        guard.observe(liberado_executor::SUBMIT_REPORT_TOOL, true, "{}"),
        ProgressAction::Continue { nudge: None }
    ));
    assert!(
        guard.fatal().is_some(),
        "the report ends the run; it does not end the stall"
    );
}

/// A FAILED edit is not a successful mutation either, latched stall or not.
#[test]
fn a_failed_edit_does_not_clear_the_stall() {
    let mut guard = ProgressGuard::new(policy());
    latch_read_only_stall(&mut guard);
    guard.observe("read_file", true, "{}");
    guard.observe("write_file", false, "io error");
    assert!(
        guard.fatal().is_some(),
        "only a successful mutation ends a read-only stall"
    );
}

#[test]
fn unknown_tools_do_not_advance_the_read_only_counter() {
    // Same-tool churn legitimately tracks unknown tools (distinct names here keep
    // that quiet); the read-only counter must not see them at all.
    let mut guard = ProgressGuard::new(policy());
    let tools = ["mystery_a", "mystery_b", "mystery_c", "mystery_d"];
    for round in 0..2 {
        for t in tools {
            let action = guard.observe(t, true, "{}");
            assert!(
                matches!(action, ProgressAction::Continue { nudge: None }),
                "round {round} {t}: unclassified tools are not read-only exploration: {action:?}"
            );
        }
    }
    assert!(guard.fatal().is_none());
}

#[test]
fn validate_does_not_count_as_plain_inspect_for_unknown_tool_routing() {
    // `validate` has its own handler; the counter branch must not treat a
    // non-validate tool as validate (inverted comparison).
    let mut guard = ProgressGuard::new(policy());
    guard.observe("write_file", true, "{}"); // neutralise read-only tracking
    let action = guard.observe("search_text", true, "{}");
    assert!(
        matches!(action, ProgressAction::Continue { nudge: None }),
        "first inspect must not trip validation machinery: {action:?}"
    );
}

/// After the nudge has fired at the limit, a call that does not advance the
/// counter must not re-fire it — the nudge is once per stall, not per call.
/// (`run_command` would advance the counter — it inspects — so the probe here
/// is a tool outside both lists.)
#[test]
fn the_read_only_nudge_does_not_repeat_without_counter_movement() {
    let mut guard = ProgressGuard::new(policy());
    guard.observe("list_files", true, "{}");
    let second = guard.observe("read_file", true, "{}");
    assert!(
        matches!(second, ProgressAction::Continue { nudge: Some(_) }),
        "{second:?}"
    );
    let action = guard.observe("mystery_probe", true, "{}");
    assert!(
        matches!(action, ProgressAction::Continue { nudge: None }),
        "counter did not move; the same nudge must not fire twice: {action:?}"
    );
}

#[test]
fn the_same_tool_nudge_does_not_repeat_through_a_multiplex_call() {
    let mut guard = ProgressGuard::new(policy());
    guard.observe("write_file", true, "{}");
    guard.observe("read_file", true, "{}");
    let second = guard.observe("read_file", true, "{}");
    assert!(matches!(
        second,
        ProgressAction::Continue { nudge: Some(_) }
    ));
    let action = guard.observe("run_command_background", true, r#"{{"exit_code":0}}"#);
    assert!(
        matches!(action, ProgressAction::Continue { nudge: None }),
        "{action:?}"
    );
}

/// Between the nudge (at the limit) and the fatal (at twice the limit) there is
/// a grace band; a fatal there would cut the model off one call early.
#[test]
fn same_tool_fatal_waits_until_twice_the_limit() {
    let mut guard = ProgressGuard::new(policy());
    guard.observe("write_file", true, "{}"); // silence read-only tracking
    assert!(matches!(
        guard.observe("read_file", true, "{}"),
        ProgressAction::Continue { nudge: None }
    ));
    assert!(matches!(
        guard.observe("read_file", true, "{}"),
        ProgressAction::Continue { nudge: Some(_) }
    ));
    let third = guard.observe("read_file", true, "{}");
    assert!(
        matches!(third, ProgressAction::Continue { nudge: None }),
        "grace band between nudge and fatal: {third:?}"
    );
    assert!(matches!(
        guard.observe("read_file", true, "{}"),
        ProgressAction::Fatal(ProgressFatal::SameToolChurn { consecutive: 4, .. })
    ));
}

#[test]
fn passing_validations_never_accumulate_churn() {
    let mut guard = ProgressGuard::new(isolated_policy());
    let pass = r#"{"passed":true,"exit_code":0,"stdout":"all green","stderr":""}"#;
    for i in 0..6 {
        let action = guard.observe("validate", true, pass);
        assert!(
            matches!(action, ProgressAction::Continue { nudge: None }),
            "pass #{i} must not count toward churn: {action:?}"
        );
    }
    assert!(guard.fatal().is_none());
}

#[test]
fn plain_text_pass_markers_are_recognised_without_json() {
    assert!(validation_passed("verify: \"passed\":true done"));
    assert!(validation_passed("verify: \"passed\": true done"));
    assert!(!validation_passed(r#"{"passed":false}"#));
    assert!(!validation_passed("no marker here"));
}

#[test]
fn distinct_validation_failures_do_not_look_like_churn() {
    let mut guard = ProgressGuard::new(isolated_policy());
    let fails = [
        r#"{"passed":false,"exit_code":1,"stdout":"missing foo","stderr":""}"#,
        r#"{"passed":false,"exit_code":2,"stdout":"","stderr":"bar blew up"}"#,
        r#"{"passed":false,"exit_code":7,"stdout":"third shape","stderr":""}"#,
        r#"{"passed":false,"exit_code":9,"stdout":"fourth","stderr":"x"}"#,
    ];
    for (i, f) in fails.iter().enumerate() {
        let action = guard.observe("validate", true, f);
        assert!(
            matches!(action, ProgressAction::Continue { nudge: None }),
            "distinct failure #{i} restarts the streak: {action:?}"
        );
    }
    assert!(guard.fatal().is_none());
}

#[test]
fn the_signature_carries_exit_stdout_and_stderr() {
    let sig = validation_signature(r#"{"exit_code":3,"stdout":"out-line","stderr":"err-line"}"#);
    assert_eq!(sig, "exit=3|stdout=out-line|stderr=err-line");
    assert_eq!(
        validation_signature("plain text"),
        "plain text",
        "non-JSON previews are their own signature"
    );
}
