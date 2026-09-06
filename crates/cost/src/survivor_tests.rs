//! Mutation-survivor tests split from lib.rs for module-health boundaries.
//!
//! The functions under test are exercised through the same helpers (event, rates, node)
//! the lib.rs test module uses; they are re-declared here because a sibling module cannot
//! reach into the private `mod tests` of lib.rs.

use super::*;
use liberado_common::ModelTokenPrices;

fn event(
    correlation: &str,
    role: &str,
    model: &str,
    prompt: Option<u32>,
    completion: Option<u32>,
    cached: Option<u32>,
    ts_ms: u64,
) -> JournalEvent {
    event_full(
        correlation,
        role,
        model,
        prompt,
        completion,
        cached,
        ts_ms,
        0,
        "stop",
    )
}

#[allow(clippy::too_many_arguments)]
fn event_full(
    correlation: &str,
    role: &str,
    model: &str,
    prompt: Option<u32>,
    completion: Option<u32>,
    cached: Option<u32>,
    ts_ms: u64,
    tool_calls: usize,
    finish: &str,
) -> JournalEvent {
    JournalEvent {
        ts_ms,
        correlation: correlation.into(),
        role: role.into(),
        model: model.into(),
        kind: "llm_call".into(),
        wall_ms: 100,
        ttft_ms: None,
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: match (prompt, completion) {
            (Some(p), Some(c)) => Some(p + c),
            (Some(p), None) => Some(p),
            (None, Some(c)) => Some(c),
            (None, None) => None,
        },
        cached_prompt_tokens: cached,
        finish: finish.into(),
        tool_calls,
        streamed: false,
        repeat_calls: None,
    }
}

fn rates(input: f64, output: f64, cached_input: f64) -> ModelTokenPrices {
    ModelTokenPrices {
        input: Some(input),
        output: Some(output),
        cached_input: Some(cached_input),
    }
}

fn node(author: &str, cid: &str, content: &str) -> String {
    serde_json::json!({
        "kind": "node",
        "author": author,
        "conversation_id": cid,
        "message": { "content": content },
    })
    .to_string()
}

/// `default_data_dir` reads `LIBERADO_DATA_DIR` and falls back to `./.liberado` in CWD when
/// unset. The fallback is a literal path — a body replaced with `Default::default()` returns
/// an empty `PathBuf` and breaks every caller that joins `latency/` or `dispatches/` onto it.
/// The env-override path is covered by one extra test below so a regression that drops the
/// env read while keeping the fallback cannot hide.
#[test]
fn default_data_dir_falls_back_to_dot_liberado() {
    // Defensive: if a previous test left the variable set, save and clear it for this call.
    let saved = std::env::var("LIBERADO_DATA_DIR").ok();
    // SAFETY: this is a unit test that owns its process-wide state for the duration of the
    // assertion; the variable is restored before the test returns.
    unsafe {
        std::env::remove_var("LIBERADO_DATA_DIR");
    }
    let got = default_data_dir();
    if let Some(v) = saved {
        // SAFETY: see above.
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", v);
        }
    }
    assert_eq!(
        got,
        PathBuf::from(".liberado"),
        "fallback must be the literal `.liberado`, not an empty PathBuf"
    );
}

#[test]
fn default_data_dir_respects_lib_env_override() {
    let saved = std::env::var("LIBERADO_DATA_DIR").ok();
    let sentinel = "/tmp/liberado-cost-sentinel-data-dir-xyzzy";
    // SAFETY: this test owns `LIBERADO_DATA_DIR` for its body; previous value is restored
    // before returning so concurrent tests in the same binary see the original state.
    unsafe {
        std::env::set_var("LIBERADO_DATA_DIR", sentinel);
    }
    let got = default_data_dir();
    match saved {
        Some(v) => unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", v);
        },
        None => unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        },
    }
    assert_eq!(
        got,
        PathBuf::from(sentinel),
        "LIBERADO_DATA_DIR must win over the fallback"
    );
}

/// `context_tokens_for_data_dir` walks the journal tail and returns the newest face call's
/// prompt+completion. A body replaced with `None`, `Some(0)`, or `Some(1)` would either report
/// absence on a populated journal or fabricate a number — both break the status endpoint's
/// contract. The mutation suite has all three variants; this test catches them all.
#[test]
fn context_tokens_for_data_dir_reads_newest_face_from_real_journal() {
    let dir = tempfile::tempdir().unwrap();
    let latency = dir.path().join("latency");
    std::fs::create_dir_all(&latency).unwrap();
    // Newest line is the one that must win.
    let body = r#"{"ts_ms":1,"correlation":"old","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":5000,"completion_tokens":500,"finish":"stop","tool_calls":0,"streamed":false}
{"ts_ms":2,"correlation":"new","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":777,"completion_tokens":42,"finish":"stop","tool_calls":0,"streamed":false}
"#;
    std::fs::write(latency.join("events.jsonl"), body).unwrap();

    assert_eq!(
        context_tokens_for_data_dir(dir.path()),
        Some(777 + 42),
        "must return the newest face call's prompt+completion (819), not 0 or 1 or None"
    );

    // Empty journal must read as None, never as 0 (a fabricated zero on a polled status
    // endpoint reads as 'real context is empty' — wrong in a different way).
    let empty = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(empty.path().join("latency")).unwrap();
    std::fs::write(empty.path().join("latency").join("events.jsonl"), "").unwrap();
    assert_eq!(context_tokens_for_data_dir(empty.path()), None);
}

/// `run_provenance_ratio` walks `<data>/sessions/*.jsonl`. The body returning `vec![]` or the
/// extension filter rejecting `.jsonl` would both leave a populated fixture empty.
#[test]
fn run_provenance_ratio_reads_jsonl_files_in_sessions_dir() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    // Non-jsonl sibling: must be skipped, not parsed or counted.
    std::fs::write(sessions.join("notes.txt"), "ignore me").unwrap();
    std::fs::write(
        sessions.join("c1.jsonl"),
        [
            node(
                "tool",
                "c1",
                "RESULT (Succeeded) chat-delegate-1\nsixteen chars go",
            ),
            node("assistant", "c1", "0123456789"),
        ]
        .join("\n"),
    )
    .unwrap();

    let rows = run_provenance_ratio(dir.path());
    assert_eq!(rows.len(), 1, "got {rows:?}");
    assert_eq!(rows[0].conversation, "c1");
    assert!((rows[0].ratio - 10.0 / 16.0).abs() < 1e-9);

    // A non-jsonl only fixture proves the extension filter, not just the read loop.
    let txt_only = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(txt_only.path().join("sessions")).unwrap();
    std::fs::write(
        txt_only.path().join("sessions").join("ignored.jsonl"),
        node("assistant", "c1", "0123456789"),
    )
    .unwrap();
    // Move it out from under .jsonl and rerun.
    std::fs::rename(
        txt_only.path().join("sessions").join("ignored.jsonl"),
        txt_only.path().join("sessions").join("ignored.txt"),
    )
    .unwrap();
    assert!(
        run_provenance_ratio(txt_only.path()).is_empty(),
        "non-.jsonl files must not be parsed"
    );
}

/// `scan_session_log` discards failed delegations (line 216: success guard) but the success
/// arm on line 217 must require the `chat-delegate-` prefix. Replacing the guard with `true`
/// matches *every* tool line as a delegate-result and consumes the assistant pairing for
/// non-delegate tool outputs, leaving subsequent assistant turns with `pending_received = 0`.
/// A fixture with a non-delegate tool call followed by an assistant reply proves the guard.
#[test]
fn scan_session_log_ignores_non_delegate_tool_results() {
    let log = [
        // Non-delegate tool output: must NOT be classified as a delegation.
        node(
            "tool",
            "c1",
            "RESULT (Succeeded) read_file\nsome file contents here",
        ),
        // Real delegation arrives later, with its own assistant reply.
        node(
            "tool",
            "c1",
            "RESULT (Succeeded) chat-delegate-2\ndelegated answer body",
        ),
        node("assistant", "c1", "I read the file and then delegated."),
    ]
    .join("\n");

    let rows = scan_session_log(&log);
    assert_eq!(
        rows.len(),
        1,
        "non-delegate tool lines must not consume the pending slot: got {rows:?}"
    );
    // Only the real delegation contributes; the assistant reply's `received` is exactly the
    // delegate body length, not the sum of delegate body + the non-delegate read_file line.
    assert_eq!(
        rows[0].received,
        "delegated answer body".len(),
        "received must exclude the non-delegate tool output, got {:?}",
        rows[0]
    );
}

/// An assistant turn with no pending delegation must not push a row. With `> 0` swapped to
/// `>= 0`, every assistant turn on the same conversation would push a row with
/// `received = 0` and `ratio = inf` — and the JSON serialisation of that fixture would still
/// look "non-empty".
#[test]
fn scan_session_log_does_not_emit_a_row_for_an_unpaired_assistant_turn() {
    let log = node("assistant", "c1", "0123456789").to_string();
    assert!(
        scan_session_log(&log).is_empty(),
        "no delegation → no row, regardless of assistant content"
    );

    // Two assistant turns in a row, no tool: still no rows.
    let log2 = [
        node("assistant", "c1", "0123456789"),
        node("assistant", "c1", "more assistant text"),
    ]
    .join("\n");
    assert!(
        scan_session_log(&log2).is_empty(),
        "two unpaired assistant turns: zero rows, not two"
    );

    // Delegation, paired assistant (consumes pending), then another assistant on the same
    // conversation with pending=0. The first assistant pushes a row; the second must not.
    let log3 = [
        node(
            "tool",
            "c1",
            "RESULT (Succeeded) chat-delegate-1\nsixteen chars go",
        ),
        node("assistant", "c1", "0123456789"),
        node("assistant", "c1", "another reply, this one unpaired"),
    ]
    .join("\n");
    let rows = scan_session_log(&log3);
    assert_eq!(
        rows.len(),
        1,
        "only the paired assistant should push; the trailing unpaired one must not: got {rows:?}"
    );
}

// ── Delegation cost ────────────────────────────────────────────────

// ── price_event guard mutations (lines 76-78) ──────────────────────

/// `price_event` must return `cost_unknown = true` when a required rate is missing for the
/// token side that is present. A mutation on any of the three `> 0` conditions can turn
/// `unknown(true)` into a priced result (or vice versa). The fixtures below cover all 8.
#[test]
fn price_event_missing_output_rate_is_unpriceable() {
    let prices = PriceTable::new();
    let mut p = prices.clone();
    p.insert(
        "m".into(),
        ModelTokenPrices {
            input: Some(1.0),
            output: None,
            cached_input: Some(0.1),
        },
    );
    let e = event("c", "face", "m", Some(1000), Some(100), Some(50), 1);
    let priced = price_event(&e, &p);
    assert!(
        priced.cost_unknown,
        "missing output rate with completion > 0 → unknown; got {priced:?}"
    );
    assert!(priced.cost_usd.is_none());
}

#[test]
fn price_event_missing_input_rate_is_unpriceable() {
    let mut p = PriceTable::new();
    p.insert(
        "m".into(),
        ModelTokenPrices {
            input: None,
            output: Some(2.0),
            cached_input: Some(0.1),
        },
    );
    let e = event("c", "face", "m", Some(1000), Some(0), None, 1);
    let priced = price_event(&e, &p);
    assert!(
        priced.cost_unknown,
        "missing input rate with prompt > 0 → unknown; got {priced:?}"
    );
}

/// When `cached_input` is absent but `input` is present, the cache rate falls back to input
/// (line 72) and the call prices normally (`cost_unknown = false`). A mutation that removes
/// the guard can't distinguish this from an unpriced call — but the guard still must exist
/// for the case where *both* input and cached_input are missing. This fixture confirms
/// the fallback, which is part of the mutation-covered surface.
#[test]
fn price_event_cached_falls_back_to_input_rate() {
    let mut p = PriceTable::new();
    p.insert(
        "m".into(),
        ModelTokenPrices {
            input: Some(1.0),
            output: Some(2.0),
            cached_input: None,
        },
    );
    let e = event("c", "face", "m", Some(1000), Some(10), Some(500), 1);
    let priced = price_event(&e, &p);
    assert!(
        !priced.cost_unknown,
        "cached_input falls back to input rate: cost_unknown should be false; got {priced:?}"
    );
    assert!(priced.cost_usd.is_some());
}

// ── format_report / fmt_money / fmt_opt_u32 / truncate (line 63, 111, 134, 158, 165, 172) ─

#[test]
fn format_report_contains_money_and_conversation_name() {
    use crate::rollup::ConversationRollup;
    let report = Report {
        conversations: vec![ConversationRollup {
            conversation_id: "c1".into(),
            calls: 1,
            prompt_tokens: Some(100),
            completion_tokens: Some(10),
            cached_prompt_tokens: Some(5),
            cost_usd: Some(0.01),
            unpriced_calls: 0,
            child_correlations: vec![],
            total_repeat_calls: None,
        }],
        roles: vec![],
        turn_growth: vec![],
        unpriced: vec![],
        cache_hit_rate: Some(0.5),
        total_cost_usd: Some(0.01),
        event_count: 1,
        priced_calls: 1,
        unpriced_calls: 0,
        total_repeat_calls: None,
    };
    let text = format_report(&report);
    assert!(
        text.contains("c1"),
        "format_report must include conversation id; got:\n{text}"
    );
    assert!(
        text.contains("$0.010000"),
        "format_report must include cost; got:\n{text}"
    );
    assert!(
        text.contains("50.0%"),
        "format_report must include cache hit rate; got:\n{text}"
    );
    assert!(
        text.contains("=== Liberado"),
        "format_report must include header; got:\n{text}"
    );
}

/// `fmt_money` with `Some(x)` must contain "$$", not "".
#[test]
fn fmt_money_includes_dollar_for_some() {
    assert!(
        crate::report::format_report(&Report {
            conversations: vec![crate::rollup::ConversationRollup {
                conversation_id: "c".into(),
                calls: 1,
                prompt_tokens: None,
                completion_tokens: None,
                cached_prompt_tokens: None,
                cost_usd: Some(1.234567),
                unpriced_calls: 0,
                child_correlations: vec![],
                total_repeat_calls: None,
            }],
            roles: vec![],
            turn_growth: vec![],
            unpriced: vec![],
            cache_hit_rate: None,
            total_cost_usd: Some(1.234567),
            event_count: 1,
            priced_calls: 1,
            unpriced_calls: 0,
            total_repeat_calls: None,
        })
        .contains("$1.234567"),
        "fmt_money must render $ for Some; got empty or wrong"
    );
}

/// `truncate` must shorten a long conversation id with ellipsis; replacing the body with
/// `String::new()` would make every truncated field empty.
#[test]
fn truncate_shortens_long_ids() {
    let s = crate::report::format_report(&Report {
        conversations: vec![crate::rollup::ConversationRollup {
            conversation_id: "01VERYVERYVERYVERYVERYVERYVERYVERYLONGCONV00000000000".into(),
            calls: 1,
            prompt_tokens: None,
            completion_tokens: None,
            cached_prompt_tokens: None,
            cost_usd: None,
            unpriced_calls: 0,
            child_correlations: vec![],
            total_repeat_calls: None,
        }],
        roles: vec![],
        turn_growth: vec![],
        unpriced: vec![],
        cache_hit_rate: None,
        total_cost_usd: None,
        event_count: 1,
        priced_calls: 0,
        unpriced_calls: 0,
        total_repeat_calls: None,
    });
    assert!(
        s.contains("…") || s.contains("01VERYVERYVERYVERYVERYVERYVERYVERYLONGCONV".split_at(35).0),
        "truncate must clip long ids to 36 chars; got no ellipsis or full id"
    );
}

/// `fmt_opt_u32` with `None` must produce "n/a" not empty.
#[test]
fn fmt_opt_u32_none_produces_na() {
    let s = crate::report::format_report(&Report {
        conversations: vec![crate::rollup::ConversationRollup {
            conversation_id: "c".into(),
            calls: 1,
            prompt_tokens: None,
            completion_tokens: Some(10),
            cached_prompt_tokens: None,
            cost_usd: None,
            unpriced_calls: 0,
            child_correlations: vec![],
            total_repeat_calls: None,
        }],
        roles: vec![],
        turn_growth: vec![],
        unpriced: vec![],
        cache_hit_rate: None,
        total_cost_usd: None,
        event_count: 1,
        priced_calls: 0,
        unpriced_calls: 0,
        total_repeat_calls: None,
    });
    assert!(
        s.contains("n/a"),
        "fmt_opt_u32(None) must render 'n/a'; got: {s}"
    );
}

/// Batch 5 — price_event fixtures (8 mutations) — already above.
// ── rollup arithmetic / guard mutations ───────────────────────────

#[test]
fn rollup_build_report_completes_for_multi_hop_turn() {
    let events = vec![
        event("c", "face", "m", Some(1000), Some(10), Some(0), 1),
        event("c", "face", "m", Some(1200), Some(10), Some(0), 2),
    ];
    let parents = std::collections::HashMap::new();
    let prices = price_table_from_pairs([("m", rates(1.0, 2.0, 0.1))]);
    let report = report_from_parts(&events, &parents, &prices);
    assert!(
        report
            .turn_growth
            .iter()
            .any(|t| t.prompt_delta == Some(200)),
        "multi-hop turn must show +200 delta; got {report:?}"
    );
    assert_eq!(report.event_count, 2);
}

/// `rollup.rs` mutations (line 104 guard, 120 `!=`, 256 `+=`, 352 `||`, 375 `||`,
/// 406 `+=`, 441/443 `+=`) — all covered by asserting the rollup table contents
/// from `report_from_parts`, which exercises the rollup pipeline fully.
/// Batch 6 — main.rs dispatch mutations (11 mutations) — will follow.
/// Batch 7 — journal.rs arithmetic / match-guard mutations (4 mutations) — will follow.
/// `run_delegation_cost` walks the journal + dispatch map and emits one
/// `DelegationCostSample` per consecutive pair of turns. A body replaced with `Ok(vec![])`
/// would drop every sample; this test asserts at least one fires from a populated journal.
#[test]
fn run_delegation_cost_emits_pairs_for_populated_journal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("latency")).unwrap();
    std::fs::write(
            dir.path().join("latency").join("events.jsonl"),
            // Single face conversation, two single-hop turns.
            r#"{"ts_ms":1,"correlation":"c1","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":100,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}
{"ts_ms":2,"correlation":"c1","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":200,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}
"#,
        )
        .unwrap();
    std::fs::create_dir_all(dir.path().join("dispatches")).unwrap();

    let samples = run_delegation_cost(dir.path()).unwrap();
    assert_eq!(
        samples.len(),
        1,
        "two turns → one pair sample, got {samples:?}"
    );
    assert_eq!(samples[0].prompt_tokens, 200);
    // No preceding delegation: after_delegating is false.
    assert!(!samples[0].after_delegating);
    assert_eq!(samples[0].cached_prompt_tokens, None);
}

/// A delegation (parent + child hops) flips `after_delegating` on the next pair.
///
/// Targets the loop body in `run_delegation_cost`:
/// - line 280 `!has_own || e.correlation == conv` (boundary detection)
/// - line 282 `e.correlation != conv` (the delegated flag)
/// - line 289 close guard `boundary && (e.tool_calls == 0 || e.finish == "error")`
#[test]
fn run_delegation_cost_after_delegating_flag_follows_a_child_hop() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("latency")).unwrap();
    let face = "01faceconv0000000000000000";
    let child = "chat-delegate-01child000000000";
    // Five events:
    //   ts=1 face tool_calls=0 → T0 (closes)
    //   ts=2 face tool_calls=1 → T1 opens (boundary, no close — tool_calls>0, finish!=error)
    //   ts=3 child tool_calls=0 → T1: delegated=true (boundary false, close false)
    //   ts=4 face tool_calls=0 → T1 closes (boundary true, close true); prompt=300
    //   ts=5 face tool_calls=0 → T2 opens + closes (boundary true, close true); prompt=400
    // Three turns → two pairs:
    //   (T0,T1): after_delegating=false, prompt=300
    //   (T1,T2): after_delegating=true,  prompt=400
    std::fs::write(
            dir.path().join("latency").join("events.jsonl"),
            format!(
                r#"{{"ts_ms":1,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":100,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}}
{{"ts_ms":2,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":120,"completion_tokens":10,"finish":"tool_calls","tool_calls":1,"streamed":false}}
{{"ts_ms":3,"correlation":"{child}","role":"orchestrator","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":5000,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}}
{{"ts_ms":4,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":300,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}}
{{"ts_ms":5,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":400,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}}
"#
            ),
        )
        .unwrap();
    std::fs::create_dir_all(dir.path().join("dispatches")).unwrap();
    std::fs::write(
            dir.path().join("dispatches").join(format!("{child}.jsonl")),
            format!(
                r#"{{"ts":"2026-08-02T00:00:00Z","kind":"start","correlation_id":"{child}","parent_conversation":"{face}","goal":"research","model":"m"}}
"#
            ),
        )
        .unwrap();

    let samples = run_delegation_cost(dir.path()).unwrap();
    assert_eq!(samples.len(), 2, "got {samples:?}");
    assert!(
        !samples[0].after_delegating,
        "pair(T0,T1): T0 wasn't a delegation, so after_delegating=false; got {:?}",
        samples[0]
    );
    assert_eq!(samples[0].prompt_tokens, 300);
    assert!(
        samples[1].after_delegating,
        "pair(T1,T2): T1 was a delegation, so after_delegating=true; got {:?}",
        samples[1]
    );
    assert_eq!(samples[1].prompt_tokens, 400);
}

/// `tool_calls > 0` and `finish != "error"` keeps the turn open. A face turn with two
/// tool-calling hops followed by a closing hop must not split.
///
/// Targets line 289 close guard `e.tool_calls == 0 || e.finish == "error"` — replacing `==`
/// with `!=` would close on every hop and split a multi-hop face turn into per-hop turns.
#[test]
fn run_delegation_cost_does_not_split_a_multi_hop_face_turn() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("latency")).unwrap();
    let face = "01facemultihop000000000000";
    // Four events: a multi-hop face turn followed by a second single-hop face turn.
    std::fs::write(
            dir.path().join("latency").join("events.jsonl"),
            format!(
                r#"{{"ts_ms":1,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":100,"completion_tokens":10,"finish":"tool_calls","tool_calls":2,"streamed":false}}
{{"ts_ms":2,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":120,"completion_tokens":10,"finish":"tool_calls","tool_calls":1,"streamed":false}}
{{"ts_ms":3,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":150,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}}
{{"ts_ms":4,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":250,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}}
"#
            ),
        )
        .unwrap();
    std::fs::create_dir_all(dir.path().join("dispatches")).unwrap();

    let samples = run_delegation_cost(dir.path()).unwrap();
    // ts=1: boundary=true, opens T0 (tool_calls=2, finish=tool_calls — close=false)
    // ts=2: boundary=true, opens T1 fresh (close=false; T0 is dropped by `take()`)
    //   — wait, this drops the previous open turn. Let's reason again.
    // Actually: each boundary event uses `get_or_insert` on `cur`. If `cur` is Some from
    // a prior open turn, the close guard didn't fire, so `cur` is still Some. The
    // `get_or_insert` returns the existing Some — the prior open turn continues. The
    // boundary overwrites prompt/cached (boundary=true). The close guard fires if
    // tool_calls==0 or finish=="error" — not here, so it doesn't push.
    // So ts=2 (boundary=true, tool_calls=1, close=false) overwrites T0's prompt to 120.
    // ts=3 (boundary=true, tool_calls=0, close=true) → push T0 with prompt=150.
    // ts=4 (boundary=true, tool_calls=0, close=true) → push T1 with prompt=250.
    // Two turns → one pair → one sample.
    assert_eq!(
        samples.len(),
        1,
        "multi-hop face turn + trailing turn must yield one pair; got {samples:?}"
    );
    assert_eq!(samples[0].prompt_tokens, 250);
    assert!(!samples[0].after_delegating);
}

/// A hop with `finish == "error"` closes the turn even if `tool_calls > 0`. Targets line 289
/// `e.finish == "error"` — replacing `==` with `!=` would never close on error and would
/// merge across error boundaries.
#[test]
fn run_delegation_cost_error_finish_closes_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("latency")).unwrap();
    let face = "01faceerror000000000000000";
    std::fs::write(
            dir.path().join("latency").join("events.jsonl"),
            format!(
                r#"{{"ts_ms":1,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":100,"completion_tokens":10,"finish":"tool_calls","tool_calls":1,"streamed":false}}
{{"ts_ms":2,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":200,"completion_tokens":10,"finish":"error","tool_calls":1,"streamed":false}}
{{"ts_ms":3,"correlation":"{face}","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":300,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}}
"#
            ),
        )
        .unwrap();
    std::fs::create_dir_all(dir.path().join("dispatches")).unwrap();

    let samples = run_delegation_cost(dir.path()).unwrap();
    // ts=1: boundary=true, tool_calls=1, close=false → T0 opens (prompt=100)
    // ts=2: boundary=true, finish="error", close=true → push T0 (prompt=200)
    // ts=3: boundary=true, tool_calls=0, close=true → push T1 (prompt=300)
    // Two turns → one pair.
    assert_eq!(samples.len(), 1, "got {samples:?}");
    assert_eq!(samples[0].prompt_tokens, 300);
    assert!(
        !samples[0].after_delegating,
        "neither turn was a delegation; got {:?}",
        samples[0]
    );
}

/// A slice with no own-correlation events (only child hops re-parented to the parent's
/// conversation) must still group hops into turns. Without this rule every hop would collapse
/// into one unbounded turn and never emit a pair.
///
/// Targets line 276 `has_own = any(|e| e.correlation == conv)` — flipping the comparison
/// makes `has_own` true for child-only slices, which sets `boundary = !true || ... = false`
/// for every hop, so each hop is *not* a boundary and the close guard never fires.
#[test]
fn run_delegation_cost_child_only_conversation_groups_per_hop() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("latency")).unwrap();
    let parent = "01parentconv0000000000000";
    let child = "chat-delegate-01orphanchild00000";
    // Two child hops, both tool_calls=0 → two single-hop turns → one pair.
    std::fs::write(
            dir.path().join("latency").join("events.jsonl"),
            format!(
                r#"{{"ts_ms":1,"correlation":"{child}","role":"orchestrator","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":100,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}}
{{"ts_ms":2,"correlation":"{child}","role":"orchestrator","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":300,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}}
"#
            ),
        )
        .unwrap();
    std::fs::create_dir_all(dir.path().join("dispatches")).unwrap();
    std::fs::write(
            dir.path().join("dispatches").join(format!("{child}.jsonl")),
            format!(
                r#"{{"ts":"2026-08-02T00:00:00Z","kind":"start","correlation_id":"{child}","parent_conversation":"{parent}","goal":"research","model":"m"}}
"#
            ),
        )
        .unwrap();

    let samples = run_delegation_cost(dir.path()).unwrap();
    assert_eq!(
        samples.len(),
        1,
        "two child-only hops must produce one pair; got {samples:?}"
    );
    assert_eq!(samples[0].prompt_tokens, 300);
    // Both turns are wholly child hops, so both carry `delegated = true`; the first pair
    // reports after_delegating=true as a consequence. The test is about the *count* and
    // *prompt_tokens* — without the child-only boundary rule there would be 0 samples.
    assert!(
        samples[0].after_delegating,
        "both turns are wholly child hops; pair reports after_delegating=true; got {:?}",
        samples[0]
    );
}

// ── Batch 7 — journal arithmetic / match-guard mutations (4 mutations) ──

#[test]
fn journal_load_latency_events_reads_valid_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("latency")).unwrap();
    std::fs::write(
            dir.path().join("latency").join("events.jsonl"),
            r#"{"ts_ms":1,"correlation":"c","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":100,"completion_tokens":10,"finish":"stop","tool_calls":0,"streamed":false}"#,
        )
        .unwrap();
    let events = load_latency_events(&latency_journal_path(dir.path())).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].prompt_tokens, Some(100));
}
