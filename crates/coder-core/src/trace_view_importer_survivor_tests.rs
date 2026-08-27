//! Split from `trace_view.rs` for module-health boundaries.

//! Survivor tests for the foreign-import path and the divergence report.

use super::*;
use crate::{CoderRunRequest, CoderTask, WorkspaceRef};
use chrono::Utc;

fn export(messages: Value) -> MessagesExport {
    MessagesExport {
        session_id: "foreign-1".into(),
        messages: messages.as_array().cloned().unwrap(),
    }
}

// ── run_view_from_messages ───────────────────────────────────────────────

#[test]
fn user_messages_become_task_then_annotations_and_assistant_turns_index_from_one() {
    let ex = export(json!([
        {"role": "user", "content": "the task itself"},
        {"role": "assistant", "content": "working"},
        {"role": "user", "content": "env: build finished"},
        {"role": "assistant", "tool_calls": [
            {"function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}}
        ]},
        {"role": "tool", "tool_call_id": "", "name": "read_file", "content": "contents", "is_error": false},
        {"role": "assistant", "tool_calls": [
            {"function": {"name": "write_file", "arguments": "{}"}}
        ]},
        {"role": "tool", "name": "write_file", "content": "wrote", "is_error": true}
    ]));
    let view = run_view_from_messages(&ex, "Kilo");

    assert_eq!(view.source, "Kilo");
    assert_eq!(view.run_id, "foreign-1");
    assert_eq!(view.task.as_deref(), Some("the task itself"));
    assert_eq!(view.turns.len(), 3, "{view:#?}");

    // Turns number from ONE.
    assert_eq!(view.turns[0].index, 1);
    assert_eq!(view.turns[0].text.as_deref(), Some("working"));
    assert!(
        view.turns[0]
            .annotations
            .iter()
            .any(|a| a.contains("user/env") && a.contains("build finished")),
        "later user messages become annotations on the open turn: {view:#?}"
    );

    // The tool result pairs with the call by name and flips ok; is_error=true → FAILED.
    let rf = &view.turns[1].calls[0];
    assert_eq!(rf.name, "read_file");
    assert_eq!(rf.ok, Some(true));
    assert_eq!(rf.output, "contents");
    let wf = &view.turns[2].calls[0];
    assert_eq!(wf.ok, Some(false), "is_error=true must mark FAILED");

    // With a tool_call_id present, pairing goes BY NAME against unanswered calls:
    // read_file's answer must not land on the still-pending write_file slot.
    let ex3 = export(json!([
        {"role": "assistant", "content": "", "tool_calls": [
            {"function": {"name": "write_file", "arguments": "{}"}},
            {"function": {"name": "read_file", "arguments": "{}"}}
        ]},
        {"role": "tool", "tool_call_id": "c9", "name": "read_file", "content": "rf-out"}
    ]));
    let v3 = run_view_from_messages(&ex3, "Kilo");
    assert_eq!(v3.turns[0].calls[0].ok, None, "write_file still awaits");
    assert_eq!(
        v3.turns[0].calls[1].ok,
        Some(true),
        "read_file answered by name"
    );
    assert_eq!(v3.turns[0].calls[1].output, "rf-out");

    // An answered call is never re-answered: a duplicate result is dropped
    // rather than overwriting the first outcome.
    let ex4 = export(json!([
        {"role": "assistant", "content": "", "tool_calls": [
            {"function": {"name": "solo", "arguments": "{}"}}
        ]},
        {"role": "tool", "name": "solo", "content": "first"},
        {"role": "tool", "name": "solo", "content": "second"}
    ]));
    let v4 = run_view_from_messages(&ex4, "Kilo");
    assert_eq!(v4.turns[0].calls[0].output, "first");

    // An assistant message with no content records text: None, not empty string.
    let ex2 = export(json!([
        {"role": "assistant", "tool_calls": [
            {"function": {"name": "x", "arguments": ""}}
        ]}
    ]));
    let v2 = run_view_from_messages(&ex2, "Kilo");
    assert_eq!(v2.task, None);
    assert_eq!(v2.turns[0].text, None);
}

// ── diverge + format_divergence ──────────────────────────────────────────

fn run_of(id: &str, turns: Vec<TurnView>) -> RunView {
    RunView {
        source: "test".into(),
        run_id: id.into(),
        task: Some(format!("task-{id}")),
        turns,
    }
}

fn tv(index: u32, calls: Vec<CallView>) -> TurnView {
    TurnView {
        index,
        text: None,
        calls,
        finish_reason: None,
        annotations: Vec::new(),
    }
}

fn cv(name: &str, ok: Option<bool>, args: &str, output: &str) -> CallView {
    CallView {
        name: name.into(),
        arguments: args.into(),
        ok,
        output: output.into(),
    }
}

#[test]
fn divergence_short_tails_do_not_claim_skipped_turns() {
    // Two turns after the divergence point is within the shown window: the
    // report must not print a "more turn(s)" line for zero skipped turns.
    let a = run_of(
        "A",
        vec![
            tv(1, vec![cv("read", Some(true), "", "")]),
            tv(2, vec![cv("edit", Some(true), "", "")]),
            tv(3, Vec::new()),
        ],
    );
    let b = run_of(
        "B",
        vec![
            tv(1, vec![cv("read", Some(true), "", "")]),
            tv(4, vec![cv("other", None, "", "")]),
        ],
    );
    let s = format_divergence(&a, &b);
    assert!(!s.contains("more turn(s)"), "nothing was skipped: {s}");

    // Long tails compress: three shown, then an exact skip count.
    let mut long_tail = vec![tv(1, vec![cv("read", Some(true), "", "")])];
    for i in 2..=5 {
        long_tail.push(tv(i, vec![cv(&format!("step{i}"), Some(true), "", "")]));
    }
    let e = run_of("E", long_tail);
    let f = run_of(
        "F",
        vec![
            tv(1, vec![cv("read", Some(true), "", "")]),
            tv(2, vec![cv("other", Some(true), "", "")]),
        ],
    );
    let s4 = format_divergence(&e, &f);
    assert!(s4.contains("… 1 more turn(s)"), "{s4}");
    // Guard annotations emit a per-harness interventions section; without
    // guards there is none.
    assert!(
        !s4.contains("harness interventions"),
        "no guards, no interventions section: {s4}"
    );

    let mut guarded = tv(2, Vec::new());
    guarded.annotations = vec!["guard loop → halt".into()];
    let g = run_of(
        "G",
        vec![tv(1, vec![cv("read", Some(true), "", "")]), guarded],
    );
    let s5 = format_divergence(&g, &g.clone());
    assert!(s5.contains("## A harness interventions"), "{s5}");
    assert!(s5.contains("turn 2: guard loop → halt"));
}

/// An export written to a bare filename (empty parent) must skip `create_dir_all`
/// rather than fail on it.
#[test]
fn write_messages_export_accepts_a_bare_filename() {
    let name = format!("lib-tv-bare-export-{}.json", std::process::id());
    let ex = MessagesExport {
        session_id: "bare".into(),
        messages: vec![json!({"role": "user", "content": "x"})],
    };
    let result = write_messages_export(std::path::Path::new(&name), &ex);
    assert!(result.is_ok(), "{result:?}");
    assert!(std::path::Path::new(&name).exists(), "file written to cwd");
    let _ = std::fs::remove_file(&name);
}

#[test]
fn load_run_view_prefers_native_then_messages_export() {
    let dir = tempfile::tempdir().unwrap();

    // A native trace wins even when it could also parse as messages.
    let native = CoderTrace {
        session_id: "native-1".into(),
        request: CoderRunRequest {
            task: CoderTask::new("t1", "d"),
            workspace: WorkspaceRef::new("/w", "HEAD"),
            config: crate::CoderRunConfig {
                backend: crate::LIBERADO_LOOP_BACKEND.into(),
                ..serde_json::from_value(json!({
                    "backend": "liberado-loop",
                    "planner": {"model": "m"},
                    "coder": {"model": "m"},
                    "critic": {"model": "m"},
                    "sandbox": {"backend": "host_local"},
                    "command_policy": {"timeout_secs": 10, "output_max_bytes": 1024}
                }))
                .unwrap()
            },
            attempt: 0,
            prior_feedback: Vec::new(),
            strategist_directive: None,
        },
        events: vec![CoderEvent::ModelTurnFinished {
            role: "coder".into(),
            turn: 1,
            tools_offered: Vec::new(),
            message_count: 1,
            content: Some("native text".into()),
            finish_reason: "prose".into(),
            tool_calls: Vec::new(),
            prompt_tokens: 1,
            completion_tokens: 1,
            at: Utc::now(),
        }],
        result: None,
    };
    let native_path = dir.path().join("run.json");
    std::fs::write(&native_path, serde_json::to_vec(&native).unwrap()).unwrap();
    let view = load_run_view(&native_path).unwrap();
    assert_eq!(view.source, "liberado");
    assert_eq!(view.turns[0].text.as_deref(), Some("native text"));

    // Our own export shape loads through the messages path.
    let ex = MessagesExport {
        session_id: "exp".into(),
        messages: vec![json!({"role": "user", "content": "exported task"})],
    };
    let exp_path = dir.path().join("exp.messages.json");
    write_messages_export(&exp_path, &ex).unwrap();
    let view = load_run_view(&exp_path).unwrap();
    assert_eq!(view.source, "liberado-messages");
    assert_eq!(view.task.as_deref(), Some("exported task"));
}

#[test]
fn divergence_finds_the_first_disagreeing_call_and_formats_both_tails() {
    let a = run_of(
        "A",
        vec![
            tv(1, vec![cv("read", Some(true), "{}", "ok")]),
            tv(
                2,
                vec![
                    cv("edit", Some(true), "{}", "ok"),
                    cv("test", Some(true), "{}", "ok"),
                ],
            ),
        ],
    );
    let b = run_of(
        "B",
        vec![
            tv(1, vec![cv("read", Some(false), "{}", "denied")]),
            tv(9, vec![cv("bash", Some(true), "{}", "ran")]),
        ],
    );

    let d = diverge(&a, &b);
    assert_eq!(d.common_calls, 1, "only `read` agrees by name");
    assert_eq!(d.a_call.as_deref(), Some("edit"));
    assert_eq!(d.b_call.as_deref(), Some("bash"));
    assert_eq!(d.a_turn, Some(2));
    assert_eq!(d.b_turn, Some(9));

    let s = format_divergence(&a, &b);
    assert!(s.contains("# Run divergence"));
    assert!(s.contains("A: A [test] — 2 model turns, 3 tool calls"));
    assert!(
        s.contains("task: task-A") && s.contains("task: task-B"),
        "actual:\n{s}"
    );
    assert!(s.contains("## Agreed for 1 call(s)"));
    assert!(s.contains("read (A turn 1 ok, B FAILED)"));
    assert!(s.contains("after call 1: A did `edit`, B did `bash`"));
    assert!(s.contains("### A from the divergence"));
    assert!(s.contains("turn 2"));
    assert!(!s.contains("nothing in common"));

    // Identical runs agree fully and say so at the end.
    let s2 = format_divergence(&a, &a.clone());
    assert!(s2.contains("Both runs stopped calling tools at the same point."));

    // Long tails show at most three turns and then say how many were skipped;
    // guard annotations surface as a per-harness interventions section.
    let mut long_tail = vec![tv(1, vec![cv("read", Some(true), "", "")])];
    for i in 2..=5 {
        long_tail.push(tv(i, vec![cv(&format!("step{i}"), Some(true), "", "")]));
    }
    let e = run_of("E", long_tail);
    let f = run_of(
        "F",
        vec![
            tv(1, vec![cv("read", Some(true), "", "")]),
            tv(2, vec![cv("other", Some(true), "", "")]),
        ],
    );
    let s4 = format_divergence(&e, &f);
    assert!(s4.contains("… 1 more turn(s)"), "{s4}");
    assert!(
        !s4.contains("harness interventions"),
        "no guards, no interventions section: {s4}"
    );

    // With a guard annotation present the section appears, tied to its turn.
    let mut guarded = tv(2, Vec::new());
    guarded.annotations = vec!["guard loop → halt".into()];
    let g = run_of(
        "G",
        vec![tv(1, vec![cv("read", Some(true), "", "")]), guarded],
    );
    let s5 = format_divergence(&g, &g.clone());
    assert!(s5.contains("## A harness interventions"), "{s5}");
    assert!(s5.contains("turn 2: guard loop → halt"));

    // Two runs whose first calls disagree share nothing.
    let c = run_of("C", vec![tv(1, vec![cv("zzz", None, "", "")])]);
    let d = run_of("D", vec![tv(1, vec![cv("yyy", None, "", "")])]);
    let s3 = format_divergence(&c, &d);
    assert!(
        s3.contains("(nothing in common — check these are the same task)"),
        "{s3}"
    );
    assert!(s3.contains("A did `zzz`, B did `yyy`"));
}

#[test]
fn fmt_ok_renders_all_three_states() {
    assert_eq!(fmt_ok(Some(true)), " ok");
    assert_eq!(fmt_ok(Some(false)), " FAILED");
    assert_eq!(fmt_ok(None), "");
}

// ── foreign-format detection ─────────────────────────────────────────────

#[test]
fn detection_prefers_kilo_cli_then_openhands_keys_then_kilo_shapes() {
    use ForeignTraceFormat::*;
    let kilo_cli = json!({"messages": [
        {"info": {"role": "user"}, "parts": [{"type": "text", "text": "hello"}]}
    ]});
    assert_eq!(detect_foreign_format(&kilo_cli).unwrap(), KiloCli);

    // An envelope with only one of the two keys is NOT the CLI shape: it falls
    // through to the plain message-list branch instead.
    let info_only = json!({"messages": [{"info": {"role": "user"}, "text": "x"}]});
    assert_ne!(detect_foreign_format(&info_only).unwrap(), KiloCli);

    for key in ["trajectory", "history", "agent_events"] {
        let oh = json!({ key: [] });
        assert_eq!(detect_foreign_format(&oh).unwrap(), OpenHands, "{key}");
    }
    for key in ["apiConversationHistory", "api_conversation_history"] {
        let k = json!({ key: [] });
        assert_eq!(detect_foreign_format(&k).unwrap(), Kilo, "{key}");
    }
    assert_eq!(
        detect_foreign_format(&json!([{"role": "user"}])).unwrap(),
        Kilo
    );
    assert_eq!(
        detect_foreign_format(&json!({"messages": [{"role":"u"}]})).unwrap(),
        Kilo
    );
    assert!(detect_foreign_format(&json!({"neither": 1})).is_err());

    // Auto round-trips each shape through its importer without error.
    assert!(import_foreign_auto(&kilo_cli, "s").is_ok());
    assert!(
        import_foreign_auto(
            &json!({"trajectory": [
                {"action": "message", "args": {"content": "x"}}
            ]}),
            "s"
        )
        .is_ok()
    );
}

// ── Kilo CLI part expansion ──────────────────────────────────────────────

#[test]
fn kilo_cli_tool_part_expands_to_call_plus_synthesized_result() {
    let raw = json!({
        "messages": [
            {"info": {"role": "user"}, "parts": [{"type": "text", "text": "go"}]},
            {"info": {"role": "assistant"}, "parts": [
                {"type": "tool", "tool": "read_file", "callID": "c1",
                 "state": {"status": "completed", "input": {"path": "a"}, "output": "bytes"}}
            ]},
            {"info": {"role": "assistant"}, "parts": [
                {"type": "tool", "tool": "run_command", "callID": "c2",
                 "state": {"status": "error", "input": {}, "error": "denied"}}
            ]}
        ]
    });
    let (_, ex) = import_foreign_auto(&raw, "kc").unwrap();
    let roles: Vec<&str> = ex
        .messages
        .iter()
        .filter_map(|m| m.get("role").and_then(|r| r.as_str()))
        .collect();
    // One part becomes an assistant call plus a synthesized role:"tool" reply.
    assert_eq!(
        roles,
        ["user", "assistant", "tool", "assistant", "tool"],
        "{ex:?}"
    );

    // The user's text part survives as content — an inverted emptiness guard
    // would blank every text message.
    assert_eq!(ex.messages[0]["content"], json!("go"));

    // The errored call carries state.error as its output body.
    let err_tool = ex
        .messages
        .iter()
        .find(|m| m.get("is_error").and_then(|v| v.as_bool()) == Some(true))
        .expect("errored part flagged");
    assert_eq!(err_tool["content"], "denied");
}

// ── OpenHands event mapping ──────────────────────────────────────────────

#[test]
fn kilo_status_and_openhands_success_flag_drive_is_error() {
    // Kilo tool message with status:"error" and no explicit is_error: the
    // normalizer must infer is_error=true. An assistant message carrying a
    // stray `error` key must NOT be flagged.
    let raw = json!({"messages": [
        {"role": "assistant", "content": "note", "error": "unrelated"},
        {"role": "tool", "name": "t", "content": "out", "status": "error"}
    ]});
    let ex = import_foreign_messages(&raw, ForeignTraceFormat::Kilo, "s").unwrap();
    assert!(
        ex.messages[0].get("is_error").and_then(|v| v.as_bool()) != Some(true),
        "assistant prose must not inherit an error flag"
    );
    assert_eq!(ex.messages[1]["is_error"], json!(true));

    // OpenHands: success=true means the tool SUCCEEDED.
    let raw = json!({"trajectory": [
        {"action": "observation", "tool_name": "ls", "content": "a", "success": true},
        {"action": "observation", "tool_name": "ls", "content": "b", "success": false}
    ]});
    let (_, ex2) = import_foreign_auto(&raw, "oh").unwrap();
    assert_eq!(ex2.messages[0]["is_error"], json!(false));
    assert_eq!(ex2.messages[1]["is_error"], json!(true));
}

#[test]
fn openhands_actions_map_onto_our_message_shapes() {
    let raw = json!({"trajectory": [
        {"action": "message", "args": {"content": "do it"}},
        {"action": "CmdRunAction", "args": {"command": "ls"}},
        {"action": "observation", "tool_name": "ls", "content": "a.txt", "success": true},
        {"action": "agent", "args": {"thought": "thinking"}}
    ]});
    let (fmt, ex) = import_foreign_auto(&raw, "oh").unwrap();
    assert_eq!(fmt, ForeignTraceFormat::OpenHands);
    let roles: Vec<String> = ex
        .messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or("?").to_string())
        .collect();
    assert_eq!(roles, ["user", "assistant", "tool", "assistant"]);
    assert!(ex.messages[1]["tool_calls"][0]["function"]["name"] == "ls");
    assert_eq!(ex.messages[2]["content"], "a.txt");
}
