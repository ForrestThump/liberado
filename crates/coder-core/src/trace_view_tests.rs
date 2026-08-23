//! Split from `trace_view.rs` for module-health boundaries.

use super::*;
use crate::{
    CoderRunConfig, CoderRunRequest, CoderRunResult, CoderTask, LIBERADO_LOOP_BACKEND, WorkspaceRef,
};
use chrono::Utc;
use liberado_common::Outcome;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture_config() -> CoderRunConfig {
    serde_json::from_value(serde_json::json!({
        "backend": "liberado-loop",
        "planner": {"model": "m"},
        "coder": {"model": "m"},
        "critic": {"model": "m"},
        "sandbox": {"backend": "host_local"},
        "command_policy": {"timeout_secs": 10, "output_max_bytes": 1024}
    }))
    .expect("config fixture")
}

fn trace_with(session_id: &str, description: &str, events: Vec<CoderEvent>) -> CoderTrace {
    CoderTrace {
        session_id: session_id.into(),
        request: CoderRunRequest {
            task: CoderTask::new("t1", description),
            workspace: WorkspaceRef::new("/w", "HEAD"),
            config: {
                let mut c = fixture_config();
                c.backend = LIBERADO_LOOP_BACKEND.into();
                c.trace_dir = None;
                c.trace_formats = Vec::new();
                c
            },
            attempt: 0,
            prior_feedback: Vec::new(),
            strategist_directive: None,
        },
        events,
        result: None,
    }
}

fn turn(n: u32, content: Option<&str>, offered: &[&str], calls: &[&str]) -> CoderEvent {
    CoderEvent::ModelTurnFinished {
        role: "coder".into(),
        turn: n,
        tools_offered: offered.iter().map(|s| s.to_string()).collect(),
        message_count: 3,
        content: content.map(str::to_string),
        finish_reason: if calls.is_empty() {
            "prose".into()
        } else {
            "tool_calls".into()
        },
        tool_calls: calls.iter().map(|s| s.to_string()).collect(),
        prompt_tokens: 10,
        completion_tokens: 2,
        at: Utc::now(),
    }
}

fn tool(name: &str, ok: bool, preview: &str) -> CoderEvent {
    CoderEvent::ToolFinished {
        name: name.into(),
        ok,
        result_preview: preview.into(),
        at: Utc::now(),
    }
}

fn tmp_dir() -> PathBuf {
    let mut p = std::env::temp_dir();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("liberado-trace-view-{nanos}"));
    fs::create_dir_all(&p).unwrap();
    p
}

// ── F1 ──────────────────────────────────────────────────────────────────

#[test]
fn render_includes_offered_tools_model_text_calls_results_and_guards() {
    let t = trace_with(
        "sess-render",
        "add a button",
        vec![
            turn(
                1,
                Some("I'll read the file first."),
                &["read_file", "write_file", "edit_file"],
                &["read_file"],
            ),
            tool("read_file", true, "fn main() {}"),
            CoderEvent::LoopGuardTriggered {
                guard: "read_only_stall".into(),
                action: "withdraw write_file".into(),
                at: Utc::now(),
            },
            turn(
                2,
                Some("Now I write."),
                &["read_file", "edit_file"],
                &["write_file"],
            ),
            tool(
                "write_file",
                false,
                "PROGRESS GUARD (fatal): stop exploring",
            ),
        ],
    );

    let text = render_transcript(&t);

    assert!(
        text.contains("tools offered:"),
        "must list offered tools: {text}"
    );
    assert!(
        text.contains("read_file") && text.contains("write_file"),
        "offered tools must appear: {text}"
    );
    assert!(
        text.contains("I'll read the file first."),
        "model text must appear: {text}"
    );
    assert!(
        text.contains("→ read_file") || text.contains("tool calls:"),
        "tool calls section required: {text}"
    );
    assert!(
        text.contains("tool result [ok]: read_file"),
        "successful tool result must appear: {text}"
    );
    assert!(
        text.contains("tool result [FAILED]: write_file"),
        "failed tool result must appear: {text}"
    );
    assert!(
        text.contains("PROGRESS GUARD"),
        "failure preview must appear: {text}"
    );
    assert!(
        text.contains("guard triggered: read_only_stall"),
        "loop guard events must appear: {text}"
    );
    assert!(
        text.contains("withdraw write_file"),
        "guard action must appear: {text}"
    );
}

/// The two things that break `liberado coder trace <session-id>` together, and only together:
/// real traces are written as `<id>-attempt-N-<stamp>.json` (so only the prefix branch can
/// match them), and the CLI searches one directory under two spellings. Scanning it twice
/// collected the same file twice and reported the only trace on disk as "ambiguous".
#[test]
fn resolves_attempt_suffixed_trace_when_a_dir_is_searched_under_two_spellings() {
    let dir = tmp_dir();
    let real_name = "01KZJ8YSMEEZ5DSQEV4Y16GGFZ-attempt-0-20260809T032711.668Z.json";
    let path = dir.join(real_name);
    let t = trace_with(
        "01KZJ8YSMEEZ5DSQEV4Y16GGFZ-attempt-0-20260809T032711.668Z",
        "cold review",
        vec![turn(1, Some("hi"), &["read_file"], &["read_file"])],
    );
    fs::write(&path, serde_json::to_vec_pretty(&t).unwrap()).unwrap();

    // Same directory, two spellings — what `default_trace_dirs()` hands us for any cwd that
    // actually contains `coder-traces`.
    let canonical = fs::canonicalize(&dir).unwrap();
    let resolved = resolve_trace_path(
        "01KZJ8YSMEEZ5DSQEV4Y16GGFZ",
        &[dir.as_path(), canonical.as_path()],
    )
    .expect("a session id with one trace on disk must resolve, not report ambiguity");
    assert_eq!(
        fs::canonicalize(&resolved).unwrap(),
        fs::canonicalize(&path).unwrap()
    );

    // Two genuinely different sessions sharing a prefix are still ambiguous — the dedupe must
    // not paper over a real collision.
    let other = dir.join("01KZJ8YSMEEZ5DSQEV4Y16GGFZ-attempt-1-20260809T041500.000Z.json");
    fs::write(&other, serde_json::to_vec_pretty(&t).unwrap()).unwrap();
    let err = resolve_trace_path(
        "01KZJ8YSMEEZ5DSQEV4Y16GGFZ",
        &[dir.as_path(), canonical.as_path()],
    )
    .expect_err("two distinct traces sharing the prefix are genuinely ambiguous");
    assert!(err.contains("ambiguous"), "{err}");
    assert_eq!(
        err.matches("attempt-").count(),
        2,
        "each real match listed once, not once per search-dir spelling: {err}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_and_render_round_trip_from_disk() {
    let dir = tmp_dir();
    let path = dir.join("s1.json");
    let t = trace_with(
        "s1",
        "wire it",
        vec![
            turn(1, Some("hello model"), &["read_file"], &["read_file"]),
            tool("read_file", true, "ok body"),
        ],
    );
    let bytes = serde_json::to_vec_pretty(&t).unwrap();
    fs::write(&path, bytes).unwrap();

    let loaded = load_trace(&path).expect("load");
    let text = render_transcript(&loaded);
    assert!(text.contains("hello model"));
    assert!(text.contains("tool result [ok]: read_file"));
    assert!(text.contains("s1"));

    let resolved = resolve_trace_path("s1", &[&dir]).expect("resolve by id");
    assert_eq!(resolved, path);

    let _ = fs::remove_dir_all(&dir);
}

// ── F2 ──────────────────────────────────────────────────────────────────

#[test]
fn compare_reports_turns_offers_refusals_mutation_and_terminal() {
    let a = {
        let mut t = trace_with(
            "run-a",
            "task A",
            vec![
                turn(
                    1,
                    Some("explore"),
                    &["read_file", "search_text"],
                    &["read_file"],
                ),
                tool("read_file", true, "src"),
                turn(
                    2,
                    Some("mutate"),
                    &["read_file", "write_file"],
                    &["write_file"],
                ),
                tool("write_file", true, "wrote"),
                CoderEvent::SessionFinished {
                    outcome: Outcome::Succeeded,
                    at: Utc::now(),
                },
            ],
        );
        t.result = Some(CoderRunResult {
            backend: "liberado-loop".into(),
            outcome: Outcome::Succeeded,
            summary: "done A".into(),
            files_changed: vec!["a.rs".into()],
            file_changes: Vec::new(),
            validation_notes: None,
            critic_verdict: None,
            gate_votes: Vec::new(),
            trace_path: None,
            diff_findings: Vec::new(),
            session_findings: Vec::new(),
            remediation: None,
            diagnostics: json!({}),
        });
        t
    };
    let b = {
        let mut t = trace_with(
            "run-b",
            "task B",
            vec![
                turn(1, Some("try write early"), &["write_file"], &["write_file"]),
                tool("write_file", false, "refused by path policy"),
                turn(2, Some("read only"), &["read_file"], &["read_file"]),
                tool("read_file", true, "x"),
                turn(3, Some("give up"), &["read_file"], &[]),
                CoderEvent::SessionFinished {
                    outcome: Outcome::Failed,
                    at: Utc::now(),
                },
            ],
        );
        t.result = Some(CoderRunResult {
            backend: "liberado-loop".into(),
            outcome: Outcome::Failed,
            summary: "stuck".into(),
            files_changed: vec![],
            file_changes: Vec::new(),
            validation_notes: None,
            critic_verdict: None,
            gate_votes: Vec::new(),
            trace_path: None,
            diff_findings: Vec::new(),
            session_findings: Vec::new(),
            remediation: None,
            diagnostics: json!({}),
        });
        t
    };

    let c = compare_traces(&a, &b);
    let report = format_comparison(&c);

    assert_eq!(c.turns_used.a, 2);
    assert_eq!(c.turns_used.b, 3);
    assert!(
        c.tools_offered_per_turn[0].a.contains(&"read_file".into()),
        "A turn1 offers read_file"
    );
    assert!(
        c.tools_offered_per_turn[0].b.contains(&"write_file".into()),
        "B turn1 offers write_file"
    );
    assert_eq!(c.refused_or_failed_calls.a.len(), 0);
    assert_eq!(c.refused_or_failed_calls.b.len(), 1);
    assert_eq!(c.refused_or_failed_calls.b[0].name, "write_file");
    assert_eq!(c.first_successful_mutation_turn.a, Some(2));
    assert_eq!(c.first_successful_mutation_turn.b, None);
    assert!(
        c.terminal
            .a
            .outcome
            .as_deref()
            .unwrap()
            .contains("Succeeded"),
        "{:?}",
        c.terminal.a
    );
    assert!(
        c.terminal.b.outcome.as_deref().unwrap().contains("Failed"),
        "{:?}",
        c.terminal.b
    );

    // Report text must surface all five metric families.
    for needle in [
        "Turns used",
        "Tools offered per turn",
        "Refused / failed calls",
        "First successful mutation",
        "Terminal cause",
    ] {
        assert!(
            report.contains(needle),
            "comparison report missing '{needle}':\n{report}"
        );
    }
    assert!(report.contains("run-a") && report.contains("run-b"));
}

/// The shape of every real run in `coder-traces/`: the model edits through `run_command`, so
/// no mutation *tool* ever fires, and the pack emits all `FileChanged` events in one batch
/// after the loop ends (`coder-agent/src/lib.rs`). Dating the first mutation from those events
/// named the run's **last** turn — a 30-turn run read as "explored 29 turns, then wrote".
#[test]
fn teardown_file_changed_batch_does_not_date_the_first_mutation() {
    let mut t = trace_with(
        "run-shell-edits",
        "edit via shell",
        vec![
            turn(
                1,
                Some("look"),
                &["read_file", "run_command"],
                &["read_file"],
            ),
            tool("read_file", true, "src"),
            turn(
                2,
                Some("patch"),
                &["read_file", "run_command"],
                &["run_command"],
            ),
            tool("run_command", true, "applied"),
            turn(
                3,
                Some("check"),
                &["read_file", "run_command"],
                &["run_command"],
            ),
            tool("run_command", true, "cargo check ok"),
            // Emitted only at teardown — after the last turn, for every file the run touched.
            CoderEvent::FileChanged {
                path: "crates/daemon/src/lib.rs".into(),
                at: Utc::now(),
            },
            CoderEvent::FileChanged {
                path: "crates/daemon/src/react.rs".into(),
                at: Utc::now(),
            },
            CoderEvent::SessionFinished {
                outcome: Outcome::Succeeded,
                at: Utc::now(),
            },
        ],
    );
    t.result = Some(CoderRunResult {
        backend: "liberado-loop".into(),
        outcome: Outcome::Succeeded,
        summary: "done".into(),
        files_changed: vec!["crates/daemon/src/lib.rs".into()],
        file_changes: Vec::new(),
        validation_notes: None,
        critic_verdict: None,
        gate_votes: Vec::new(),
        trace_path: None,
        diff_findings: Vec::new(),
        session_findings: Vec::new(),
        remediation: None,
        diagnostics: json!({}),
    });

    let c = compare_traces(&t, &t);

    assert_eq!(
        c.first_successful_mutation_turn.a, None,
        "a teardown FileChanged batch carries no turn timing and must not invent one"
    );
    assert_ne!(
        c.first_successful_mutation_turn.a,
        Some(c.turns_used.a),
        "reporting the last turn as the first mutation is the defect, not the fix"
    );
    assert_eq!(
        c.files_changed.a, 2,
        "the run did mutate; the count is what the trace can honestly say"
    );

    let report = format_comparison(&c);
    assert!(
        report.contains("no mutation-tool call") && report.contains("2 file(s) changed"),
        "report must say it cannot date the mutation, and that files changed anyway:\n{report}"
    );
    assert!(
        !report.contains("First successful mutation (turn)\n  A: 3"),
        "report must not name a turn it cannot know:\n{report}"
    );
}

// ── Cross-harness run view + divergence ─────────────────────────────────

/// A trace's call sequence must be what the *model asked for*, not what the runtime happened to
/// trace. `scratchpad_write` emits no `ToolStarted`/`ToolFinished` at all — five of thirty-five
/// calls in the real trace on disk — so a sequence built from tool events alone drops them and
/// shifts every later alignment by one.
#[test]
fn run_view_keeps_calls_that_emitted_no_tool_events() {
    let t = trace_with(
        "untraced",
        "do the thing",
        vec![
            // Requested, and it produces no tool events (as scratchpad_write does not).
            turn(
                1,
                Some("note it"),
                &["scratchpad_write"],
                &["scratchpad_write"],
            ),
            turn(2, Some("now read"), &["read_file"], &["read_file"]),
            tool("read_file", true, "fn main() {}"),
        ],
    );

    let view = run_view_from_trace(&t);
    let names: Vec<&str> = view
        .turns
        .iter()
        .flat_map(|turn| turn.calls.iter().map(|c| c.name.as_str()))
        .collect();
    assert_eq!(
        names,
        vec!["scratchpad_write", "read_file"],
        "an untraced call is still a call the model made: {view:#?}"
    );
    assert_eq!(
        view.turns[0].calls[0].ok, None,
        "and it is honestly marked as having no recorded result"
    );
    assert_eq!(view.turns[1].calls[0].ok, Some(true));
    assert_eq!(view.turns[1].calls[0].output, "fn main() {}");
    assert_eq!(
        view.turns[1].calls.len(),
        1,
        "the result fills the seeded call rather than duplicating it"
    );
}

/// The alignment rule itself, on the case that distinguishes it: two runs that make the *same
/// two calls* but package them into different numbers of turns.
///
/// This is not hypothetical — the Kilo CLI closes a separate assistant message for its final
/// `stop`, and our runs batch parallel calls into one turn, so turn *N* on one side is
/// routinely not turn *N* on the other. Aligning by turn index reports these two runs as
/// diverging after one call when they in fact agreed on both, which is a wrong answer
/// delivered confidently. The two runs are constructed rather than loaded because the point is
/// to isolate the rule; the end-to-end path is covered against a real export separately.
#[test]
fn alignment_is_by_call_sequence_not_turn_index() {
    let call = |name: &str| CallView {
        name: name.into(),
        arguments: String::new(),
        ok: Some(true),
        output: String::new(),
    };
    let turn_with = |index: u32, calls: Vec<CallView>| TurnView {
        index,
        text: None,
        calls,
        finish_reason: None,
        annotations: Vec::new(),
    };

    // Both did `read` then `search` — one in a single batched turn, the other one per turn.
    let batched = RunView {
        source: "liberado".into(),
        run_id: "batched".into(),
        task: Some("same task".into()),
        turns: vec![
            turn_with(1, vec![call("read"), call("search")]),
            turn_with(2, vec![call("run_command")]),
        ],
    };
    let sequential = RunView {
        source: "KiloCli".into(),
        run_id: "sequential".into(),
        task: Some("same task".into()),
        turns: vec![
            turn_with(1, vec![call("read")]),
            turn_with(2, vec![call("search")]),
            turn_with(3, vec![call("edit")]),
        ],
    };

    let d = diverge(&batched, &sequential);
    assert_eq!(
        d.common_calls, 2,
        "both agreed on `read` then `search`; only the turn packaging differs: {d:?}"
    );
    assert_eq!(d.a_call.as_deref(), Some("run_command"));
    assert_eq!(d.b_call.as_deref(), Some("edit"));
    // And the turns reported are each run's own numbering at that point, which differ.
    assert_eq!(d.a_turn, Some(2));
    assert_eq!(d.b_turn, Some(3));
}

/// The cross-harness path end to end, with a real `kilo export` on one side: alignment is by
/// call sequence, and the report names where the two parted and what each did next.
#[test]
fn divergence_aligns_by_call_sequence_across_harnesses() {
    let ours = run_view_from_trace(&trace_with(
        "ours",
        "read hello.txt",
        vec![
            turn(1, Some("read it"), &["read"], &["read"]),
            tool("read", true, "liberado"),
            // Where we go wrong: a second call the other harness never makes.
            turn(2, Some("check again"), &["run_command"], &["run_command"]),
            tool("run_command", false, "error: could not compile"),
            CoderEvent::LoopGuardTriggered {
                guard: "read_only_stall".into(),
                action: "withdraw write_file".into(),
                at: Utc::now(),
            },
        ],
    ));

    let kilo: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/kilo-cli-export-read-ok.json"
    ))
    .expect("fixture parses");
    let (_, export) = import_foreign_auto(&kilo, "kilo").expect("import");
    let theirs = run_view_from_messages(&export, "KiloCli");

    let d = diverge(&ours, &theirs);
    assert_eq!(
        d.common_calls, 1,
        "both ran `read` first, so the shared prefix is one call: {d:?}"
    );
    assert_eq!(d.a_call.as_deref(), Some("run_command"));
    assert_eq!(
        d.b_call, None,
        "the other harness simply stopped calling tools"
    );

    let report = format_divergence(&ours, &theirs);
    assert!(report.contains("Agreed for 1 call(s)"), "{report}");
    assert!(
        report.contains("error: could not compile"),
        "the failing output is the answer to 'why did ours fail' and must be in the report: \
             {report}"
    );
    assert!(
        report.contains("guard read_only_stall"),
        "a harness intervention with no counterpart is the likeliest cause, not noise: {report}"
    );
    assert!(
        report.contains("liberado") && report.contains("KiloCli"),
        "both sources named so a reader can tell which side is which: {report}"
    );
}

// ── F3 ──────────────────────────────────────────────────────────────────

#[test]
fn import_kilo_conversation_history_to_messages_json() {
    let kilo = json!([
        {"role": "user", "content": "fix the watcher"},
        {
            "role": "assistant",
            "content": "I'll inspect the file.",
            "tool_calls": [{
                "type": "function",
                "function": {"name": "read_file", "arguments": "{\"path\":\"a.rs\"}"}
            }]
        },
        {
            "role": "tool",
            "name": "read_file",
            "content": "fn main() {}",
            "is_error": false
        },
        {"role": "assistant", "content": "done"}
    ]);

    let export =
        import_foreign_messages(&kilo, ForeignTraceFormat::Kilo, "kilo-1").expect("kilo import");
    assert_eq!(export.session_id, "kilo-1");
    assert_eq!(export.messages.len(), 4);
    assert_eq!(export.messages[0]["role"], "user");
    assert_eq!(export.messages[0]["content"], "fix the watcher");
    assert_eq!(export.messages[1]["role"], "assistant");
    assert_eq!(
        export.messages[1]["tool_calls"][0]["function"]["name"],
        "read_file"
    );
    assert_eq!(export.messages[2]["role"], "tool");
    assert_eq!(export.messages[2]["is_error"], false);
    assert_eq!(export.messages[3]["content"], "done");
}

/// A fixture in Kilo's **real** on-disk shape, taken from the reader shipped in Kilo Code
/// 7.4.20 (`dist/extension.js`): a bare JSON array of Anthropic messages, `user`/`assistant`
/// roles only, tool activity carried as `tool_use` / `tool_result` content blocks.
///
/// The wrong implementation this excludes is the one that was here: reading the file as
/// OpenAI-shaped (`tool_calls` fields, `role: "tool"` entries). That version parses this input
/// without error and returns prose with every tool call and result missing — so the assertions
/// below are on the tool activity, which is the only part that can tell the two apart.
#[test]
fn import_kilo_anthropic_blocks_keeps_tool_calls_and_results() {
    let kilo = json!([
        {
            "role": "user",
            "content": [{"type": "text", "text": "<task>fix the watcher</task>"}],
            "ts": 1_754_700_000_000i64
        },
        {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "I'll read the file first."},
                {
                    "type": "tool_use",
                    "id": "toolu_01A",
                    "name": "read_file",
                    "input": {"path": "crates/daemon/src/vault_source.rs"}
                }
            ]
        },
        {
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_01A",
                    "content": [{"type": "text", "text": "pub fn react() {}"}]
                },
                {"type": "text", "text": "<environment_details># VSCode Visible Files</environment_details>"}
            ]
        },
        {
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_01B",
                    "name": "apply_diff",
                    "input": {"path": "a.rs", "diff": "-x\n+y"}
                }
            ]
        },
        {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_01B",
                "content": "ERROR: no match found",
                "is_error": true
            }]
        }
    ]);

    let export =
        import_foreign_messages(&kilo, ForeignTraceFormat::Kilo, "kilo-real").expect("import");
    let m = &export.messages;

    // The assistant's tool call survives, with its arguments.
    let call = &m[1]["tool_calls"][0];
    assert_eq!(call["function"]["name"], "read_file", "messages: {m:#?}");
    assert_eq!(call["id"], "toolu_01A");
    let args: Value = serde_json::from_str(call["function"]["arguments"].as_str().unwrap())
        .expect("arguments must be a JSON string, as OpenAI carries them");
    assert_eq!(args["path"], "crates/daemon/src/vault_source.rs");

    // The result comes back as a tool message, named from the `tool_use` it answers.
    assert_eq!(m[2]["role"], "tool");
    assert_eq!(m[2]["name"], "read_file");
    assert_eq!(m[2]["tool_call_id"], "toolu_01A");
    assert_eq!(m[2]["content"], "pub fn react() {}");
    assert_eq!(m[2]["is_error"], false);

    // Text alongside a tool result stays a user turn, and is not merged into the result.
    assert_eq!(m[3]["role"], "user");
    assert!(
        m[3]["content"]
            .as_str()
            .unwrap()
            .contains("environment_details"),
        "what the model saw is kept verbatim: {m:#?}"
    );

    // A tool-call-only assistant entry is still a turn; a failed result keeps its error flag.
    assert_eq!(m[4]["role"], "assistant");
    assert_eq!(m[4]["tool_calls"][0]["function"]["name"], "apply_diff");
    assert_eq!(m[5]["role"], "tool");
    assert_eq!(m[5]["name"], "apply_diff");
    assert_eq!(m[5]["is_error"], true);

    let roles: Vec<&str> = m.iter().filter_map(|x| x["role"].as_str()).collect();
    assert_eq!(
        roles,
        vec!["user", "assistant", "tool", "user", "assistant", "tool"],
        "every turn accounted for, none invented"
    );
}

/// Verbatim `kilo export <sessionID>` output from Kilo CLI 7.4.11 — a real run of
/// "Read hello.txt and tell me the single word inside it" against `deepseek-v4-flash`, and a
/// real failed `read` of a missing path. Not hand-built: the point of these two files is that
/// they were produced by the harness rather than by our idea of it, so the importer is held to
/// what Kilo actually writes.
///
/// The wrong implementations they exclude: reading a `kilo export` as an Anthropic/OpenAI
/// message list (it has no `role` at top level — that failed outright), and reading a tool
/// part as call-only (dropping `state.output`, which is the entire result side of the run).
#[test]
fn import_real_kilo_cli_export_keeps_calls_results_and_errors() {
    let ok: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/kilo-cli-export-read-ok.json"
    ))
    .expect("fixture parses");

    let (format, export) = import_foreign_auto(&ok, "kilo-cli-ok").expect("auto-detect+import");
    assert_eq!(
        format,
        ForeignTraceFormat::KiloCli,
        "a kilo export must not be mistaken for the extension's message list"
    );

    let roles: Vec<&str> = export
        .messages
        .iter()
        .filter_map(|m| m["role"].as_str())
        .collect();
    assert_eq!(
        roles,
        vec!["user", "assistant", "tool", "assistant"],
        "one tool part expands into a call on the assistant turn plus its reply: {:#?}",
        export.messages
    );

    let call = &export.messages[1]["tool_calls"][0];
    assert_eq!(call["function"]["name"], "read");
    assert_eq!(call["id"], "call_00_ET_8S0lzpuMcpKVo6nqM3ix9191");
    let args: Value = serde_json::from_str(call["function"]["arguments"].as_str().unwrap())
        .expect("arguments serialized as a JSON string");
    assert!(
        args["filePath"].as_str().unwrap().ends_with("hello.txt"),
        "the call's real arguments survive: {args}"
    );

    let result = &export.messages[2];
    assert_eq!(result["name"], "read");
    assert_eq!(
        result["tool_call_id"],
        "call_00_ET_8S0lzpuMcpKVo6nqM3ix9191"
    );
    assert_eq!(result["is_error"], false);
    assert!(
        result["content"].as_str().unwrap().contains("liberado"),
        "the tool's real output survives: {result}"
    );

    // A failed call: Kilo writes `status: "error"` with the message in `state.error` and no
    // `output` key at all, so reading only `output` would report an empty successful call.
    let failed: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/kilo-cli-export-read-error.json"
    ))
    .expect("fixture parses");
    let export = import_foreign_messages(&failed, ForeignTraceFormat::KiloCli, "kilo-cli-err")
        .expect("import");
    let errored = export
        .messages
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the failed call still produces a tool message");
    assert_eq!(errored["is_error"], true, "{errored}");
    assert!(
        errored["content"]
            .as_str()
            .unwrap()
            .contains("File not found"),
        "the failure reason survives: {errored}"
    );
}

#[test]
fn import_openhands_trajectory_to_messages_json() {
    let oh = json!({
        "trajectory": [
            {"action": "message", "args": {"content": "implement feature X"}},
            {
                "action": "run",
                "args": {"command": "read_file", "path": "src/lib.rs"}
            },
            {
                "action": "run_observation",
                "tool_name": "read_file",
                "content": "pub fn x() {}",
                "success": true
            },
            {
                "action": "agent",
                "args": {"content": "Feature complete."}
            }
        ]
    });

    let export = import_foreign_messages(&oh, ForeignTraceFormat::OpenHands, "oh-1")
        .expect("openhands import");
    assert_eq!(export.session_id, "oh-1");
    assert!(
        export.messages.len() >= 3,
        "expected mapped messages, got {:?}",
        export.messages
    );
    assert_eq!(export.messages[0]["role"], "user");
    assert!(
        export.messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("implement feature X")
    );
    // Somewhere an assistant tool call and a tool result should appear.
    let roles: Vec<&str> = export
        .messages
        .iter()
        .filter_map(|m| m.get("role").and_then(|r| r.as_str()))
        .collect();
    assert!(roles.contains(&"assistant"), "roles={roles:?}");
    assert!(roles.contains(&"tool"), "roles={roles:?}");
}

#[test]
fn import_writes_parseable_messages_file() {
    let dir = tmp_dir();
    let input = dir.join("api_conversation_history.json");
    let output = dir.join("imported.messages.json");
    let kilo = json!({
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi"}
        ]
    });
    let mut f = fs::File::create(&input).unwrap();
    f.write_all(serde_json::to_vec_pretty(&kilo).unwrap().as_slice())
        .unwrap();

    let (fmt, export) = import_foreign_file(&input, None, Some("imp-1".into())).unwrap();
    assert_eq!(fmt, ForeignTraceFormat::Kilo);
    write_messages_export(&output, &export).unwrap();

    let loaded: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(loaded["session_id"], "imp-1");
    assert!(
        loaded["messages"].as_array().unwrap().len() >= 2,
        "{loaded}"
    );

    let _ = fs::remove_dir_all(&dir);
}
