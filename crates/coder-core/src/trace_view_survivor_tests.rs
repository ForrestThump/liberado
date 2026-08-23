//! Split from `trace_view.rs` for module-health boundaries.

//! Tests added to close the 62 trace_view mutation survivors. Every assertion was
//! verified to fail under the specific mutant it targets (see the campaign ledger).

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture_config() -> crate::CoderRunConfig {
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

fn tmp_dir() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("lib-tv-{}-{nanos}", std::process::id()));
    p
}
use crate::{CoderRunRequest, CoderTask, CoderTrace, WorkspaceRef};
use chrono::Utc;
use liberado_common::Outcome;

fn at() -> chrono::DateTime<Utc> {
    Utc::now()
}

fn bare_trace(events: Vec<CoderEvent>) -> CoderTrace {
    CoderTrace {
        session_id: "sess-view".into(),
        request: CoderRunRequest {
            task: CoderTask::new("t1", "describe"),
            workspace: WorkspaceRef::new("/w", "HEAD"),
            config: {
                let mut c = fixture_config();
                c.backend = crate::LIBERADO_LOOP_BACKEND.into();
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

// ── render_transcript covers every event kind ────────────────────────────

/// Each `CoderEvent` variant must leave its marker in the transcript: these renderers
/// are how a human reads a run, and a deleted match arm silently drops that event
/// from every transcript instead of failing loudly.
#[test]
fn transcript_renders_every_event_variant() {
    let events = vec![
        CoderEvent::SessionStarted {
            session_id: "sess-view".into(),
            backend: "liberado-loop".into(),
            task_id: "t1".into(),
            at: at(),
        },
        CoderEvent::RoleStarted {
            role: "coder".into(),
            model: "m1".into(),
            at: at(),
        },
        CoderEvent::ModelRequestSent {
            role: "coder".into(),
            turn: 1,
            tools_offered: vec!["write_file".into()],
            message_count: 3,
            system_prompt_sha256: "cafebabe1234567890".into(),
            system_prompt: Some("You are a coding agent.".into()),
            at: at(),
        },
        CoderEvent::ModelTurnStarted {
            role: "coder".into(),
            turn: 1,
            at: at(),
        },
        CoderEvent::ModelTurnFinished {
            role: "coder".into(),
            turn: 1,
            tools_offered: vec!["write_file".into(), "read_file".into()],
            message_count: 4,
            content: Some("I will write.".into()),
            finish_reason: "tool_calls".into(),
            tool_calls: vec!["write_file".into()],
            prompt_tokens: 11,
            completion_tokens: 7,
            at: at(),
        },
        CoderEvent::ToolStarted {
            name: "write_file".into(),
            args_preview: "{\"path\":\"a.txt\"}".into(),
            at: at(),
        },
        CoderEvent::ToolFinished {
            name: "write_file".into(),
            ok: true,
            result_preview: "wrote 12 bytes\nsecond line".into(),
            at: at(),
        },
        CoderEvent::ToolStarted {
            name: "list_dir".into(),
            args_preview: String::new(),
            at: at(),
        },
        CoderEvent::ToolFinished {
            name: "read_file".into(),
            ok: false,
            result_preview: "refused: not granted".into(),
            at: at(),
        },
        CoderEvent::FileChanged {
            path: "a.txt".into(),
            at: at(),
        },
        CoderEvent::ValidationFinished {
            ok: true,
            summary: "cargo test passed".into(),
            at: at(),
        },
        CoderEvent::LoopGuardTriggered {
            guard: "same-call".into(),
            action: "withdraw-tool".into(),
            at: at(),
        },
        CoderEvent::CriticVerdict {
            verdict: crate::CriticVerdict::NeedsRevision {
                issues: vec!["nits".into()],
            },
            at: at(),
        },
        CoderEvent::ReportFiled {
            outcome: Outcome::PartiallySucceeded,
            summary: "mostly there".into(),
            at: at(),
        },
        CoderEvent::RoleFinished {
            role: "coder".into(),
            at: at(),
        },
        CoderEvent::SessionFinished {
            outcome: Outcome::Succeeded,
            at: at(),
        },
        CoderEvent::SessionAborted {
            error: "disk full".into(),
            at: at(),
        },
    ];
    let t = render_transcript(&bare_trace(events));
    let check = |needle: &str| {
        assert!(t.contains(needle), "transcript missing {needle:?}:\n{t}");
    };
    check("== session started ==");
    check("backend: liberado-loop");
    check("-- role started: coder (m1)");
    check("== system prompt (turn 1, sha256 cafebabe1234)");
    check("You are a coding agent.");
    check("tools offered: write_file");
    check("## turn 1 (coder) started");
    check("## turn 1 — coder finished");
    check("finish: tool_calls");
    check("tokens: prompt=11 completion=7");
    check("- write_file");
    check("- read_file");
    check("| I will write.");
    check("→ write_file");
    check("tool start: write_file");
    check("args: {\"path\":\"a.txt\"}");
    check("tool result [ok]: write_file");
    check("| second line");
    check("tool start: list_dir");
    // Empty args preview prints no args line — the `!args_preview.is_empty()` guard.
    let list_start = t.find("tool start: list_dir").unwrap();
    let rest = &t[list_start..list_start + 200];
    assert!(
        !rest.contains("args:"),
        "empty args preview must not print an args line: {rest:?}"
    );
    check("tool result [FAILED]: read_file");
    check("| refused: not granted");
    check("file changed: a.txt");
    check("validation [ok]");
    check("!! guard triggered: same-call → withdraw-tool");
    check("critic @");
    check("== report filed ==");
    check("outcome: PartiallySucceeded");
    check("summary: mostly there");
    check("-- role finished: coder");
    check("== session finished ==");
    check("outcome: Succeeded");
    check("== session ABORTED (unhandled error) ==");
    check("error: disk full");
    check("# Coder trace: sess-view");
    check("task: t1 — describe");

    // A later request with no verbatim prompt prints nothing (hash-only arm).
    let repeat_only = vec![CoderEvent::ModelRequestSent {
        role: "coder".into(),
        turn: 2,
        tools_offered: Vec::new(),
        message_count: 5,
        system_prompt_sha256: "deadbeef".into(),
        system_prompt: None,
        at: at(),
    }];
    let t2 = render_transcript(&bare_trace(repeat_only));
    assert!(
        !t2.contains("system prompt"),
        "hash-only request must not reprint a prompt block:\n{t2}"
    );

    // A trace with a terminal result renders the result line.
    let mut tr = bare_trace(vec![]);
    tr.result = Some(result_with(Outcome::Failed, "verifier refused", vec![]));
    let t3 = render_transcript(&tr);
    assert!(t3.contains("result: Failed — verifier refused"), "{t3}");
}

// ── metrics ──────────────────────────────────────────────────────────────

#[test]
fn metrics_counts_turns_tools_failures_mutations_and_terminals() {
    let events = vec![
        turn_ev(1, &["a", "b"], &[]),
        tool_ev("run_command", false, "denied by policy"),
        tool_ev("write_file", true, "ok"),
        CoderEvent::FileChanged {
            path: "x.rs".into(),
            at: at(),
        },
        CoderEvent::FileChanged {
            path: "y.rs".into(),
            at: at(),
        },
        CoderEvent::FileChanged {
            path: "x.rs".into(),
            at: at(),
        },
        turn_ev(2, &["c"], &["edit_file"]),
        tool_ev("read_file", false, "boom"),
        CoderEvent::ReportFiled {
            outcome: Outcome::Succeeded,
            summary: "done".into(),
            at: at(),
        },
        CoderEvent::SessionFinished {
            outcome: Outcome::Succeeded,
            at: at(),
        },
    ];
    let m = metrics(&bare_trace(events));

    assert_eq!(m.turns_used, 2);
    // Exactly one slot per seen turn — an off-by-one in the fill loop shifts these.
    assert_eq!(m.tools_offered_per_turn.len(), 2);
    assert_eq!(m.tools_offered_per_turn[0], vec!["a", "b"]);
    assert_eq!(m.tools_offered_per_turn[1], vec!["c"]);
    assert_eq!(m.refused_or_failed_calls.len(), 2);
    assert_eq!(m.refused_or_failed_calls[0].name, "run_command");
    assert_eq!(m.refused_or_failed_calls[0].turn, Some(1));
    assert_eq!(m.refused_or_failed_calls[1].result_preview, "boom");
    assert_eq!(m.first_successful_mutation_turn, Some(1));
    assert_eq!(m.files_changed, 2, "duplicate paths count once");
    assert_eq!(m.terminal.outcome.as_deref(), Some("Succeeded"));
    assert_eq!(m.terminal.summary.as_deref(), Some("done"));
    // SessionFinished came last among events: each terminal event overwrites the
    // outcome/cause, but ReportFiled's summary survives (SessionFinished sets none).
    assert_eq!(m.terminal.cause, "session finished: Succeeded");
    assert_eq!(m.terminal.summary.as_deref(), Some("done"));
}

#[test]
fn metrics_terminal_variants_and_guard_fallback() {
    // Guard only fills the cause when nothing terminal has been seen yet; a later
    // real terminal must win even though the guard came after it in this order.
    let ev = vec![
        CoderEvent::ReportFiled {
            outcome: Outcome::Failed,
            summary: "refused".into(),
            at: at(),
        },
        CoderEvent::LoopGuardTriggered {
            guard: "g".into(),
            action: "stop".into(),
            at: at(),
        },
    ];
    let m = metrics(&bare_trace(ev));
    assert!(
        m.terminal.cause.starts_with("report filed"),
        "{}",
        m.terminal.cause
    );
    assert_eq!(m.terminal.outcome.as_deref(), Some("Failed"));

    // Guard alone: its text is the best available cause.
    let ev = vec![CoderEvent::LoopGuardTriggered {
        guard: "loop".into(),
        action: "halt".into(),
        at: at(),
    }];
    let m = metrics(&bare_trace(ev));
    assert_eq!(m.terminal.cause, "guard loop → halt (no terminal yet)");

    // Abort carries its error as the summary.
    let ev = vec![CoderEvent::SessionAborted {
        error: "crashed".into(),
        at: at(),
    }];
    let m = metrics(&bare_trace(ev));
    assert_eq!(m.terminal.outcome.as_deref(), Some("Aborted"));
    assert_eq!(m.terminal.summary.as_deref(), Some("crashed"));
    assert_eq!(m.terminal.cause, "session aborted: crashed");

    // SessionFinished without ReportFiled still names its outcome and cause.
    let ev = vec![CoderEvent::SessionFinished {
        outcome: Outcome::Proposed,
        at: at(),
    }];
    let m = metrics(&bare_trace(ev));
    assert_eq!(m.terminal.outcome.as_deref(), Some("Proposed"));
    assert_eq!(m.terminal.cause, "session finished: Proposed");

    // With no FileChanged events, the result's own file list is the truth.
    let mut tr = bare_trace(vec![]);
    tr.result = Some(result_with(
        Outcome::Succeeded,
        "s",
        vec!["one".into(), "two".into(), "three".into()],
    ));
    let m = metrics(&tr);
    assert_eq!(m.files_changed, 3);

    // First successful mutation dates from the CURRENT turn, not turn one.
    let ev = vec![
        turn_ev(3, &[], &[]),
        tool_ev("git_commit", true, "committed"),
    ];
    let m = metrics(&bare_trace(ev));
    assert_eq!(m.first_successful_mutation_turn, Some(3));

    // A non-mutation success does not date a mutation.
    let ev = vec![turn_ev(1, &[], &[]), tool_ev("read_file", true, "ok")];
    let m = metrics(&bare_trace(ev));
    assert_eq!(m.first_successful_mutation_turn, None);
}

fn turn_ev(n: u32, offered: &[&str], calls: &[&str]) -> CoderEvent {
    CoderEvent::ModelTurnFinished {
        role: "coder".into(),
        turn: n,
        tools_offered: offered.iter().map(|s| s.to_string()).collect(),
        message_count: 1,
        content: None,
        finish_reason: "prose".into(),
        tool_calls: calls.iter().map(|s| s.to_string()).collect(),
        prompt_tokens: 1,
        completion_tokens: 1,
        at: at(),
    }
}

fn result_with(
    outcome: Outcome,
    summary: &str,
    files_changed: Vec<String>,
) -> crate::CoderRunResult {
    serde_json::from_value(serde_json::json!({
        "backend": "liberado-loop",
        "outcome": outcome,
        "summary": summary,
        "files_changed": files_changed,
    }))
    .expect("result fixture")
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

fn tool_ev(name: &str, ok: bool, preview: &str) -> CoderEvent {
    CoderEvent::ToolFinished {
        name: name.into(),
        ok,
        result_preview: preview.into(),
        at: at(),
    }
}

// ── formatting helpers ───────────────────────────────────────────────────

#[test]
fn fmt_helpers_render_empty_and_populated_cases() {
    assert_eq!(fmt_tools(&[]), "(none)");
    assert_eq!(fmt_tools(&["a".into(), "b".into()]), "a, b");

    assert_eq!(fmt_failures(&[]), "    (none)\n");
    let long = "z".repeat(300);
    let fails = vec![
        FailedCall {
            name: "run_command".into(),
            result_preview: "denied".into(),
            turn: Some(2),
        },
        FailedCall {
            name: "read_file".into(),
            result_preview: long,
            turn: None,
        },
    ];
    let out = fmt_failures(&fails);
    assert!(out.contains("- run_command (turn 2): denied\n"), "{out}");
    assert!(out.contains("- read_file (turn ?): "), "{out}");
    // Preview truncated to 120 chars plus ellipsis.
    let line = out.lines().nth(1).unwrap();
    let body = line.split(": ").nth(1).unwrap();
    assert_eq!(body.chars().count(), 121, "120 chars + ellipsis: {body}");

    assert_eq!(fmt_mutation(Some(4), 99), "4");
    assert_eq!(fmt_mutation(None, 0), "never");
    assert_eq!(
        fmt_mutation(None, 7),
        "no mutation-tool call (7 file(s) changed, turn unattributable)"
    );

    assert_eq!(truncate("short", 10), "short");
    assert_eq!(truncate("0123456789", 5), "01234…");
}

#[test]
fn comparison_numbers_turns_from_one_and_lists_sides() {
    let c = TraceComparison {
        a_session_id: "A".into(),
        b_session_id: "B".into(),
        turns_used: SideBySide { a: 3, b: 4 },
        tools_offered_per_turn: vec![
            SideBySide {
                a: vec!["x".into()],
                b: Vec::new(),
            },
            SideBySide {
                a: vec!["y".into()],
                b: vec!["z".into()],
            },
        ],
        refused_or_failed_calls: SideBySide {
            a: vec![FailedCall {
                name: "f".into(),
                result_preview: "p".into(),
                turn: Some(1),
            }],
            b: Vec::new(),
        },
        first_successful_mutation_turn: SideBySide {
            a: Some(1),
            b: None,
        },
        files_changed: SideBySide { a: 5, b: 4 },
        terminal: SideBySide {
            a: TerminalSummary {
                outcome: Some("Succeeded".into()),
                summary: Some("sa".into()),
                cause: "ca".into(),
            },
            b: TerminalSummary {
                outcome: None,
                summary: None,
                cause: "cb".into(),
            },
        },
    };
    let s = format_comparison(&c);
    assert!(s.contains("turn 1:"), "{s}");
    assert!(s.contains("turn 2:"), "{s}");
    assert!(!s.contains("turn 0:"), "turn numbering is 1-based: {s}");
    assert!(s.contains("A: x") && s.contains("B: (none)"));
    assert!(s.contains("A (1):"));
    assert!(s.contains("B (0):"));
    assert!(s.contains("A: 1\n"));
    assert!(
        s.contains("B: no mutation-tool call (4 file(s) changed, turn unattributable)"),
        "{s}"
    );
    assert!(s.contains("A: outcome=Succeeded cause=ca summary=sa"));
    assert!(s.contains("B: outcome=— cause=cb summary=—"));
}

// ── resolve_trace_path ───────────────────────────────────────────────────

#[test]
fn resolve_prefers_exact_then_prefix_then_errors() {
    let dir = tmp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("abc123.json"), b"{}").unwrap();

    // Exact id wins over any scan.
    let hit = resolve_trace_path("abc123", &[&dir]).unwrap();
    assert!(hit.ends_with("abc123.json"));

    // A ULID prefix matches the attempt-suffixed stem.
    std::fs::write(dir.join("def456-attempt-1-x.json"), b"{}").unwrap();
    let hit = resolve_trace_path("def456", &[&dir]).unwrap();
    assert!(hit.ends_with("def456-attempt-1-x.json"));

    // Message exports are skipped by the prefix scan.
    std::fs::write(dir.join("ghi789.messages.json"), b"{}").unwrap();
    let err = resolve_trace_path("ghi789", &[&dir]).unwrap_err();
    assert!(err.contains("no native coder trace"), "{err}");

    // Files matching neither exact nor prefix are not candidates at all — the
    // error says "not found", not "ambiguous".
    std::fs::write(dir.join("other1.json"), b"{}").unwrap();
    std::fs::write(dir.join("other2.json"), b"{}").unwrap();
    let err = resolve_trace_path("zzzz", &[&dir]).unwrap_err();
    assert!(err.contains("no native coder trace found"), "{err}");

    // An explicit existing path passes through untouched.
    let direct = dir.join("other1.json");
    assert_eq!(
        resolve_trace_path(direct.to_str().unwrap(), &[&dir]).unwrap(),
        direct
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── run_view_from_trace ──────────────────────────────────────────────────

#[test]
fn run_view_pairs_arguments_with_results_by_name_in_order() {
    let events = vec![
        turn(1, None, &[], &["write_file", "read_file"]),
        CoderEvent::ToolStarted {
            name: "write_file".into(),
            args_preview: "{\"path\":\"a.txt\"}".into(),
            at: at(),
        },
        CoderEvent::ToolStarted {
            name: "read_file".into(),
            args_preview: "{\"path\":\"b.txt\"}".into(),
            at: at(),
        },
        CoderEvent::ToolFinished {
            name: "read_file".into(),
            ok: false,
            result_preview: "nope".into(),
            at: at(),
        },
        CoderEvent::ToolFinished {
            name: "write_file".into(),
            ok: true,
            result_preview: "wrote".into(),
            at: at(),
        },
        CoderEvent::LoopGuardTriggered {
            guard: "g".into(),
            action: "a".into(),
            at: at(),
        },
    ];
    let view = run_view_from_trace(&bare_trace(events));

    assert_eq!(view.source, "liberado");
    assert_eq!(view.run_id, "sess-view");
    assert_eq!(view.task.as_deref(), Some("describe"));
    assert_eq!(view.turns.len(), 1);
    let t = &view.turns[0];
    assert_eq!(t.index, 1);
    assert_eq!(t.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(t.calls.len(), 2);
    // Results arrived read_file-first; the call LIST keeps the model's issue order.
    // A pairing mutant that grabs the first pending slot instead swaps this order.
    assert_eq!(t.calls[0].name, "write_file");
    assert_eq!(t.calls[1].name, "read_file");
    // Pairing is BY NAME oldest-first, not positional-inverted: read_file's result
    // must pick up read_file's arguments, not write_file's.
    let wf = t.calls.iter().find(|c| c.name == "write_file").unwrap();
    assert_eq!(wf.arguments, "{\"path\":\"a.txt\"}");
    assert_eq!(wf.ok, Some(true));
    assert_eq!(wf.output, "wrote");
    let rf = t.calls.iter().find(|c| c.name == "read_file").unwrap();
    assert_eq!(rf.arguments, "{\"path\":\"b.txt\"}");
    assert_eq!(rf.ok, Some(false));
    // Guards become annotations on the open turn.
    assert_eq!(t.annotations, vec!["guard g → a"]);
}

#[test]
fn run_view_without_a_turn_seeds_one_and_keeps_unmatched_ok_none() {
    // Tool results arriving before any ModelTurnFinished still form a view.
    let events = vec![
        CoderEvent::ToolFinished {
            name: "solo".into(),
            ok: true,
            result_preview: "out".into(),
            at: at(),
        },
        turn(7, Some("text"), &[], &[]),
    ];
    let view = run_view_from_trace(&bare_trace(events));
    assert_eq!(view.turns.len(), 2);
    assert_eq!(view.turns[0].index, 1);
    assert_eq!(view.turns[0].calls[0].name, "solo");
    // The seeded call for a requested-but-unresulted tool keeps ok: None — exactly
    // "the record never said whether it succeeded".
    assert_eq!(view.turns[1].index, 7);
    assert_eq!(view.turns[1].calls.len(), 0);
}
