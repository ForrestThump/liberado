use super::*;

const SAMPLE_MVL: &str = r#"
{"v":1,"type":"run_started","ts":"2026-08-11T00:00:00.000Z","run":"r1","seq":0,"harness":{"name":"liberado","version":"0.1.0"}}
{"v":1,"type":"tool_catalog","ts":"2026-08-11T00:00:00.001Z","run":"r1","seq":1,"sha256":"cat-aaa","tools":[{"name":"grep","description":"Search","input_schema":{"type":"object"}}]}
{"v":1,"type":"prompt","ts":"2026-08-11T00:00:00.002Z","run":"r1","seq":2,"turn":0,"messages":{"mode":"full","items":[{"role":"user","content":"fix it"}]},"system":{"sha256":"sys-1","text":"You are the coder."},"tool_catalog_sha256":"cat-aaa","tools_offered":["grep"],"params":{"temperature":0.0,"max_tokens":100}}
{"v":1,"type":"completion","ts":"2026-08-11T00:00:00.003Z","run":"r1","seq":3,"turn":0,"text":"searching","tool_calls":[{"id":"c1","name":"grep","arguments":{"pattern":"x"}}],"finish_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"2026-08-11T00:00:00.004Z","run":"r1","seq":4,"turn":0,"call_id":"c1","name":"grep","ok":true,"content_shown":"hit"}
{"v":1,"type":"prompt","ts":"2026-08-11T00:00:00.005Z","run":"r1","seq":5,"turn":1,"messages":{"mode":"delta","items":[{"role":"tool","content":"hit"}]},"system":{"sha256":"sys-1","text":null},"tool_catalog_sha256":"cat-aaa","tools_offered":["grep"],"params":{"temperature":0.0,"max_tokens":100}}
{"v":1,"type":"completion","ts":"2026-08-11T00:00:00.006Z","run":"r1","seq":6,"turn":1,"text":"done","tool_calls":[],"finish_reason":"stop"}
{"v":1,"type":"run_ended","ts":"2026-08-11T00:00:00.007Z","run":"r1","seq":7,"outcome":"succeeded","reason":"model finished","gates":[]}
"#;

const SAMPLE_EXEC: &str = r#"
{"v":1,"type":"attempt_started","ts":"2026-08-11T00:00:00.000Z","run":"r1","seq":0,"attempt":0,"workspace":"/ws"}
{"v":1,"type":"tool_started","ts":"2026-08-11T00:00:00.003Z","run":"r1","seq":1,"turn":0,"call_id":"c1","name":"grep"}
{"v":1,"type":"tool_finished","ts":"2026-08-11T00:00:00.004Z","run":"r1","seq":2,"turn":0,"call_id":"c1","name":"grep","ok":true,"duration_ms":12,"bytes_out":3}
{"v":1,"type":"gate_result","ts":"2026-08-11T00:00:00.006Z","run":"r1","seq":3,"attempt":0,"name":"nonempty-diff","passed":true}
{"v":1,"type":"attempt_ended","ts":"2026-08-11T00:00:00.007Z","run":"r1","seq":4,"attempt":0,"outcome":"succeeded","reason":"ok"}
"#;

#[test]
fn reconstructs_system_messages_catalog_and_params_for_turn_1() {
    let events = parse_jsonl(SAMPLE_MVL).expect("parse");
    assert_seq_gap_free(&events).expect("seq");
    let turn = reconstruct_turn(&events, 1).expect("turn 1");
    assert_eq!(turn.system_text, "You are the coder.");
    assert_eq!(turn.system_sha256, "sys-1");
    assert_eq!(turn.tool_catalog_sha256, "cat-aaa");
    assert_eq!(
        turn.tool_definitions,
        serde_json::json!([{"name":"grep","description":"Search","input_schema":{"type":"object"}}])
    );
    assert_eq!(turn.messages.len(), 2);
    assert_eq!(turn.messages[0]["role"], "user");
    assert_eq!(turn.messages[1]["role"], "tool");
    assert_eq!(
        turn.params.get("temperature").and_then(|v| v.as_f64()),
        Some(0.0)
    );
    assert_eq!(
        turn.params.get("max_tokens").and_then(|v| v.as_i64()),
        Some(100)
    );
    assert_eq!(turn.tools_offered, vec!["grep".to_string()]);
}

#[test]
fn full_prompt_resets_message_list() {
    let text = r#"
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":0,"sha256":"c","tools":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":1,"turn":0,"messages":{"mode":"full","items":[{"role":"user","content":"a"}]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":2,"turn":1,"messages":{"mode":"full","items":[{"role":"user","content":"b"}]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
    let events = parse_jsonl(text).unwrap();
    let turn = reconstruct_turn(&events, 1).unwrap();
    assert_eq!(turn.messages.len(), 1);
    assert_eq!(turn.messages[0]["content"], "b");
}

#[test]
fn missing_catalog_fails_reconstruction() {
    let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"missing","tools_offered":[],"params":{}}
"#;
    let events = parse_jsonl(text).unwrap();
    let err = reconstruct_turn(&events, 0).unwrap_err();
    assert!(err.contains("tool catalog"), "{err}");
}

#[test]
fn target_prompt_must_carry_its_request_metadata() {
    let text = r#"
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":0,"sha256":"c","tools":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":1,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{"temperature":0.0}}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":2,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":[]}
"#;
    let events = parse_jsonl(text).unwrap();
    let err = reconstruct_turn(&events, 1).unwrap_err();
    assert!(err.contains("params"), "{err}");
}

#[test]
fn prompt_after_context_change_must_be_full() {
    let text = r#"
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":0,"sha256":"c","tools":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":1,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
{"v":1,"type":"context_changed","ts":"t","run":"r","seq":2,"turn":1,"kind":"offload","removed_messages":1}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":3,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
    let events = parse_jsonl(text).unwrap();
    let err = reconstruct_turn(&events, 1).unwrap_err();
    assert!(
        err.contains("context_changed") && err.contains("full"),
        "{err}"
    );
}

#[test]
fn execution_joins_mvl_by_call_id() {
    let mvl = parse_jsonl(SAMPLE_MVL).unwrap();
    let ex = parse_jsonl(SAMPLE_EXEC).unwrap();
    assert_seq_gap_free(&ex).unwrap();
    assert_join_integrity(&mvl, &ex).unwrap();
    assert_attempt_brackets(&ex).unwrap();
    assert_mvl_has_no_scheduler_leakage(&mvl).unwrap();
}

#[test]
fn join_fails_when_execution_call_has_no_mvl() {
    let mvl = parse_jsonl(SAMPLE_MVL).unwrap();
    let bad = r#"
{"v":1,"type":"tool_started","ts":"t","run":"r1","seq":0,"turn":0,"call_id":"orphan","name":"grep"}
"#;
    let ex = parse_jsonl(bad).unwrap();
    let err = assert_join_integrity(&mvl, &ex).unwrap_err();
    assert!(err.contains("orphan"), "{err}");
}

#[test]
fn execution_call_does_not_join_an_orphan_tool_result() {
    let mvl = parse_jsonl(
        r#"{"v":1,"type":"tool_result","ts":"t","run":"r","seq":0,"turn":0,"call_id":"c1","name":"x","ok":true,"content_shown":"x"}"#,
    )
    .unwrap();
    let ex = parse_jsonl(
        r#"{"v":1,"type":"tool_started","ts":"t","run":"r","seq":0,"turn":0,"call_id":"c1","name":"x"}"#,
    )
    .unwrap();
    let err = assert_join_integrity(&mvl, &ex).unwrap_err();
    assert!(err.contains("0 MVL tool-call joins"), "{err}");
}

#[test]
fn execution_call_rejects_ambiguous_mvl_tool_calls() {
    let mvl = parse_jsonl(
        r#"
{"v":1,"type":"completion","ts":"t","run":"r","seq":0,"turn":0,"text":"","tool_calls":[{"id":"c1","name":"x","arguments":{}}],"finish_reason":"tool_calls"}
{"v":1,"type":"completion","ts":"t","run":"r","seq":1,"turn":1,"text":"","tool_calls":[{"id":"c1","name":"x","arguments":{}}],"finish_reason":"tool_calls"}
"#,
    )
    .unwrap();
    let ex = parse_jsonl(
        r#"{"v":1,"type":"tool_started","ts":"t","run":"r","seq":0,"turn":0,"call_id":"c1","name":"x"}"#,
    )
    .unwrap();
    let err = assert_join_integrity(&mvl, &ex).unwrap_err();
    assert!(err.contains("2 MVL tool-call joins"), "{err}");
}

/// Spec conformance item 1: context_transform + turn must join MVL context_changed (or a
/// following full prompt). Mutation: drop this check — a green suite would accept the old
/// non-conforming sample pair that had execution offload without an MVL counterpart.
#[test]
fn join_fails_when_context_transform_has_no_mvl_match() {
    let mvl = parse_jsonl(SAMPLE_MVL).unwrap();
    let bad = r#"
{"v":1,"type":"context_transform","ts":"t","run":"r1","seq":0,"turn":1,"kind":"offload","duration_ms":1,"removed_messages":0,"summary_bytes":0}
"#;
    let ex = parse_jsonl(bad).unwrap();
    let err = assert_join_integrity(&mvl, &ex).unwrap_err();
    assert!(
        err.contains("context_transform") && err.contains("context_changed"),
        "{err}"
    );
}

#[test]
fn context_transform_joins_via_context_changed() {
    let mvl = parse_jsonl(
        r#"
{"v":1,"type":"run_started","ts":"t","run":"r1","seq":0,"harness":{"name":"x","version":"0"}}
{"v":1,"type":"context_changed","ts":"t","run":"r1","seq":1,"turn":1,"kind":"offload","removed_messages":0}
{"v":1,"type":"prompt","ts":"t","run":"r1","seq":2,"turn":2,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#,
    )
    .unwrap();
    let ex = parse_jsonl(
        r#"
{"v":1,"type":"context_transform","ts":"t","run":"r1","seq":0,"turn":1,"kind":"offload","duration_ms":1,"removed_messages":0,"summary_bytes":0}
"#,
    )
    .unwrap();
    assert_join_integrity(&mvl, &ex).expect("context_changed joins transform");
}

#[test]
fn mvl_rejects_execution_types() {
    let bad = r#"
{"v":1,"type":"tool_started","ts":"t","run":"r","seq":0,"turn":0,"call_id":"c","name":"x"}
"#;
    let mvl = parse_jsonl(bad).unwrap();
    assert!(assert_mvl_has_no_scheduler_leakage(&mvl).is_err());
}

#[test]
fn attempt_ended_without_start_fails() {
    let bad = r#"
{"v":1,"type":"attempt_ended","ts":"t","run":"r","seq":0,"attempt":0,"outcome":"x","reason":"y"}
"#;
    let ex = parse_jsonl(bad).unwrap();
    assert!(assert_attempt_brackets(&ex).is_err());
}

#[test]
fn attempt_start_matches_only_one_end() {
    let bad = r#"
{"v":1,"type":"attempt_started","ts":"t","run":"r","seq":0,"attempt":0,"workspace":"/ws"}
{"v":1,"type":"attempt_ended","ts":"t","run":"r","seq":1,"attempt":0,"outcome":"x","reason":"y"}
{"v":1,"type":"attempt_ended","ts":"t","run":"r","seq":2,"attempt":0,"outcome":"x","reason":"y"}
"#;
    let ex = parse_jsonl(bad).unwrap();
    let err = assert_attempt_brackets(&ex).unwrap_err();
    assert!(err.contains("without unmatched"), "{err}");
}

#[test]
fn sequence_check_rejects_mixed_runs() {
    let text = r#"
{"v":1,"type":"run_started","ts":"t","run":"r1","seq":0}
{"v":1,"type":"run_ended","ts":"t","run":"r2","seq":1}
"#;
    let events = parse_jsonl(text).unwrap();
    let err = assert_seq_gap_free(&events).unwrap_err();
    assert!(err.contains("run changed"), "{err}");
}

#[test]
fn crash_survival_accepts_complete_prefix() {
    let prefix = SAMPLE_MVL
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    let events = assert_crash_survival(&prefix).expect("complete prefix");
    assert_eq!(events.len(), 4);
    assert_seq_gap_free(&events).expect("prefix seq");
}

#[test]
fn crash_survival_rejects_trailing_partial() {
    let text = format!(
        "{}\n{{\"v\":1,\"type\":\"prompt\",\"ts\":\"t\",\"run\":\"r1\",\"seq\":",
        SAMPLE_MVL.trim()
    );
    let err = assert_crash_survival(&text).unwrap_err();
    assert!(
        err.contains("crash survival") && err.contains("incomplete"),
        "{err}"
    );
}

#[test]
fn system_prompt_once_accepts_sample() {
    let events = parse_jsonl(SAMPLE_MVL).unwrap();
    assert_system_prompt_once(&events).expect("sample system");
}

#[test]
fn system_prompt_once_rejects_duplicate_full_text() {
    let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":1,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
    let events = parse_jsonl(text).unwrap();
    let err = assert_system_prompt_once(&events).unwrap_err();
    assert!(err.contains("more than once"), "{err}");
}

#[test]
fn system_prompt_once_rejects_unrecoverable_hash() {
    let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"missing","text":null},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
    let events = parse_jsonl(text).unwrap();
    let err = assert_system_prompt_once(&events).unwrap_err();
    assert!(err.contains("not recoverable"), "{err}");
}

#[test]
fn tool_catalog_once_accepts_sample() {
    let events = parse_jsonl(SAMPLE_MVL).unwrap();
    assert_tool_catalog_once(&events).expect("sample catalog");
}

#[test]
fn tool_catalog_once_rejects_duplicate_sha() {
    let text = r#"
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":0,"sha256":"c","tools":[]}
{"v":1,"type":"tool_catalog","ts":"t","run":"r","seq":1,"sha256":"c","tools":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":2,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{}}
"#;
    let events = parse_jsonl(text).unwrap();
    let err = assert_tool_catalog_once(&events).unwrap_err();
    assert!(err.contains("more than once"), "{err}");
}

#[test]
fn tool_catalog_once_rejects_unresolvable_hash() {
    let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"missing","tools_offered":[],"params":{}}
"#;
    let events = parse_jsonl(text).unwrap();
    let err = assert_tool_catalog_once(&events).unwrap_err();
    assert!(err.contains("not recoverable"), "{err}");
}

#[test]
fn withdrawal_accepts_explicit_tools_changed() {
    let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":["a","b"],"params":{}}
{"v":1,"type":"tools_changed","ts":"t","run":"r","seq":1,"turn":0,"removed":["b"],"added":[]}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":2,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":["a"],"params":{}}
"#;
    let events = parse_jsonl(text).unwrap();
    assert_tools_changed_covers_offered_diff(&events).expect("covered");
}

#[test]
fn withdrawal_rejects_offered_shrink_without_tools_changed() {
    let text = r#"
{"v":1,"type":"prompt","ts":"t","run":"r","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":["a","b"],"params":{}}
{"v":1,"type":"prompt","ts":"t","run":"r","seq":1,"turn":1,"messages":{"mode":"delta","items":[]},"system":{"sha256":"s","text":null},"tool_catalog_sha256":"c","tools_offered":["a"],"params":{}}
"#;
    let events = parse_jsonl(text).unwrap();
    let err = assert_tools_changed_covers_offered_diff(&events).unwrap_err();
    assert!(err.contains("without intervening tools_changed"), "{err}");
}

#[test]
fn honesty_accepts_matching_bytes() {
    let events = parse_jsonl(SAMPLE_MVL).unwrap();
    let mut expected = BTreeMap::new();
    expected.insert("c1".into(), "hit".into());
    assert_tool_honesty(&events, &expected).expect("honest");
}

#[test]
fn honesty_rejects_mismatched_content_shown() {
    let events = parse_jsonl(SAMPLE_MVL).unwrap();
    let mut expected = BTreeMap::new();
    expected.insert("c1".into(), "DIFFERENT".into());
    let err = assert_tool_honesty(&events, &expected).unwrap_err();
    assert!(
        err.contains("content_shown != ground truth") && err.contains("c1"),
        "{err}"
    );
}

#[test]
fn reconstruct_all_turns_covers_sample() {
    let events = parse_jsonl(SAMPLE_MVL).unwrap();
    let turns = reconstruct_all_turns(&events).expect("all turns");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].turn, 0);
    assert_eq!(turns[1].turn, 1);
    assert_eq!(turns[0].system_text, "You are the coder.");
}

/// Only `prompt` events seed turn reconstruction. The SAMPLE_MVL carries `completion`,
/// `tool_result`, and other non-prompt events with a `turn` field — cargo-mutants's
/// `==` -> `!=` mutation on the type-name check would let those events seed turns,
/// which then fail `reconstruct_turn` (no prompt) and the test sees an Err.
#[test]
fn reconstruct_all_turns_only_seeds_from_prompt_events() {
    let events = parse_jsonl(SAMPLE_MVL).unwrap();
    // The unmutated function returns Ok with 2 turns. The `!=` mutation makes
    // `reconstruct_turn` fail for turns sourced from non-prompt events; we expect
    // an error rather than a silently-shorter result.
    let result = reconstruct_all_turns(&events);
    assert!(
        result.is_ok(),
        "sample MVL has a prompt for each turn: {result:?}"
    );
}

/// A `context_transform` is joined by a `context_changed` event at the same turn.
/// The `&&` -> `||` mutation on the `if !has_changed && !has_following_full` guard
/// would error on a clean context_changed (the `!has_changed` half would flip to
/// `!has_changed`, true, and combined with `||` the whole condition would fire).
/// A two-event fixture with just the MVL `context_changed` and the matching
/// exec `context_transform` (no following full prompt) proves the `&&` is correct.
#[test]
fn context_transform_with_only_context_changed_passes_join() {
    let mvl = parse_jsonl(
        r#"
{"v":1,"type":"run_started","ts":"t","run":"r1","seq":0,"harness":{"name":"x","version":"0"}}
{"v":1,"type":"context_changed","ts":"t","run":"r1","seq":1,"turn":1,"kind":"offload","removed_messages":0}
"#,
    )
    .unwrap();
    let ex = parse_jsonl(
        r#"
{"v":1,"type":"context_transform","ts":"t","run":"r1","seq":0,"turn":1,"kind":"offload","duration_ms":1,"removed_messages":0,"summary_bytes":0}
"#,
    )
    .unwrap();
    assert_join_integrity(&mvl, &ex).expect("context_changed alone is sufficient");
}

/// The MVL leakage rule rejects `rss_bytes` and `cpu_ms` as execution-only fields.
/// cargo-mutants's `||` -> `&&` mutation requires BOTH fields to be present before
/// flagging — a single `rss_bytes` event would pass. A test with just `rss_bytes`
/// (no `cpu_ms`) pins the original OR semantics.
#[test]
fn mvl_rejects_rss_bytes_alone() {
    let mvl = parse_jsonl(
        r#"
{"v":1,"type":"prompt","ts":"t","run":"r1","seq":0,"turn":0,"messages":{"mode":"full","items":[]},"system":{"sha256":"s","text":"S"},"tool_catalog_sha256":"c","tools_offered":[],"params":{},"rss_bytes":1000}
"#,
    )
    .unwrap();
    let err = assert_mvl_has_no_scheduler_leakage(&mvl).unwrap_err();
    assert!(
        err.contains("rss_bytes") || err.contains("resource"),
        "{err}"
    );
}
