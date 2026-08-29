//! Split from `lib.rs`: kills the baseline campaign's survivors.
//!
//! Pure helpers first: spill-label sanitisation, char-boundary truncation,
//! prompt-builder wording pins, doom/cycle escalation rungs, and the
//! one-time recovery bonus.

use super::*;
use crate::loop_guard::{RunPolicy, args_similarity};
use liberado_provider::MockProvider;
use serde_json::json;
use std::path::Path;

#[test]
fn spill_labels_keep_alphanumerics_dashes_and_underscores() {
    assert_eq!(sanitize_spill_label("read_file-7"), "read_file-7");
    assert_eq!(sanitize_spill_label("a b/c.d"), "a_b_c_d");
}

#[test]
fn empty_spill_labels_fall_back_to_call() {
    assert_eq!(sanitize_spill_label(""), "call");
    // Punctuation survives as underscores; only true emptiness falls back.
    assert_eq!(sanitize_spill_label("///"), "___");
}

#[test]
fn char_boundary_walks_back_to_a_real_boundary() {
    // 'é' is two bytes; a cut of 2 lands inside it.
    let text = "héllo";
    assert_eq!(char_boundary_at_or_before(text, 2), 1);
    assert_eq!(char_boundary_at_or_before(text, 0), 0);
    assert_eq!(
        char_boundary_at_or_before(text, 99),
        text.len(),
        "an oversized index clamps to len"
    );
}

#[test]
fn truncate_head_cuts_on_char_boundaries() {
    assert_eq!(truncate_head("abcdef", 4), "abcd");
    assert_eq!(truncate_head("héllo", 2), "h", "must not split 'é'");
    assert_eq!(truncate_head("short", 99), "short");
}

#[test]
fn the_wrap_up_directive_states_the_withdrawal_and_the_reserve() {
    let d = wrap_up_directive("turns", 3);
    assert!(d.contains("run out of turns"), "{d}");
    assert!(d.contains("3 turn(s) left"), "{d}");
    assert!(d.contains(SUBMIT_REPORT_TOOL), "{d}");
    assert!(d.contains("PartiallySucceeded"), "{d}");
}

#[test]
fn the_tools_removed_nudge_names_the_removed_tools() {
    let n = tools_removed_nudge(&["search_text".into(), "list_files".into()]);
    assert!(
        n.contains("`search_text`, `list_files`"),
        "the joined list must be exact: {n}"
    );
    assert!(n.contains("submit_report"), "{n}");
}

#[test]
fn the_malformed_report_nudge_includes_the_parse_error() {
    let err = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    let n = malformed_report_nudge(&err);
    assert!(n.contains("NOT accepted"), "{n}");
    assert!(n.contains("outcome"), "{n}");
    assert!(n.contains("summary"), "{n}");
}

/// The refusal rung removes only the offending tool and tells the model why.
#[test]
fn doom_give_up_removes_only_the_offending_tool() {
    let mut tools = vec![
        ToolDef {
            name: "keep_me".into(),
            description: String::new(),
            parameters: json!({}),
        },
        ToolDef {
            name: "offender".into(),
            description: String::new(),
            parameters: json!({}),
        },
    ];
    let mut messages: Vec<Message> = Vec::new();
    doom_give_up(9, &mut tools, &mut messages, "offender");
    assert_eq!(
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["keep_me"],
        "only the offender is withdrawn"
    );
    assert!(!messages.is_empty(), "the model is told what happened");
}

#[test]
fn cycle_remove_strips_every_cycling_tool_and_grants_one_bonus() {
    let tool = |name: &str| ToolDef {
        name: name.into(),
        description: String::new(),
        parameters: json!({}),
    };
    let mut tools = vec![tool("a"), tool("b"), tool("c")];
    let mut messages: Vec<Message> = Vec::new();
    let mut bonus_granted = false;
    let mut max_turns = 30;

    cycle_remove(
        4,
        &mut tools,
        &mut messages,
        &mut bonus_granted,
        &mut max_turns,
        &["b".to_string()],
    );
    assert_eq!(
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["a", "c"],
        "every cycling tool goes"
    );
    assert!(!messages.is_empty());
    assert!(bonus_granted);
    assert_eq!(max_turns, 32, "the one-time recovery top-up is +2");
}

#[test]
fn cycle_give_up_refuses_the_cycling_set() {
    let tool = |name: &str| ToolDef {
        name: name.into(),
        description: String::new(),
        parameters: json!({}),
    };
    let mut tools = vec![tool("a"), tool("b"), tool("c")];
    let mut messages: Vec<Message> = Vec::new();
    cycle_give_up(
        5,
        &mut tools,
        &mut messages,
        &["a".to_string(), "c".to_string()],
    );
    assert_eq!(
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["b"],
        "{tools:?}"
    );
    assert!(!messages.is_empty());
}

#[test]
fn the_recovery_bonus_is_granted_once() {
    let mut granted = false;
    let mut max_turns = 10;
    grant_recovery_bonus(&mut granted, &mut max_turns);
    assert!(granted);
    assert_eq!(max_turns, 10 + DOOM_LOOP_RECOVERY_BONUS_TURNS);

    // A second escalation must not stack the bonus.
    grant_recovery_bonus(&mut granted, &mut max_turns);
    assert_eq!(max_turns, 10 + DOOM_LOOP_RECOVERY_BONUS_TURNS);
}

// ── request observation ─────────────────────────────────────────────────────

#[derive(Default)]
struct RecordingObserver {
    requests: std::sync::Mutex<Vec<RequestRecord>>,
}

impl TurnObserver for RecordingObserver {
    fn on_turn(&self, _: TurnRecord) {}
    fn on_request(&self, record: RequestRecord) {
        self.requests.lock().unwrap().push(record);
    }
}

fn system_and_user(system: &str, user: &str) -> Vec<Message> {
    vec![
        Message {
            role: Role::System,
            content: system.into(),
            tool_calls: vec![],
            tool_call_id: None,
        },
        Message {
            role: Role::User,
            content: user.into(),
            tool_calls: vec![],
            tool_call_id: None,
        },
    ]
}

#[tokio::test]
async fn the_system_prompt_is_hashed_and_forwarded_once_seen() {
    use sha2::Digest;
    let observer = Arc::new(RecordingObserver::default());
    let provider = Arc::new(MockProvider::new("mock"));
    let executor = Executor::new(provider, Budget::new(4)).with_observer(observer.clone());
    let messages = system_and_user("You are terse.", "go");
    executor.observe_request(0, &["read_file".to_string()], 2, &messages);
    let requests = observer.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let expected = format!("{:x}", sha2::Sha256::digest(b"You are terse."));
    assert_eq!(requests[0].system_prompt_sha256, expected);
    assert_eq!(
        requests[0].system_prompt.as_deref(),
        Some("You are terse."),
        "{requests:?}"
    );
}

/// A run without a system message records NO system prompt — an empty string
/// would masquerade as a real (empty) prompt.
#[tokio::test]
async fn a_missing_system_message_records_no_prompt() {
    let observer = Arc::new(RecordingObserver::default());
    let provider = Arc::new(MockProvider::new("mock"));
    let executor = Executor::new(provider, Budget::new(4)).with_observer(observer.clone());
    executor.observe_request(
        0,
        &[],
        1,
        &[Message {
            role: Role::User,
            content: "just this".into(),
            tool_calls: vec![],
            tool_call_id: None,
        }],
    );
    let requests = observer.requests.lock().unwrap();
    assert_eq!(requests[0].system_prompt, None, "{requests:?}");
}

// ── model selection ─────────────────────────────────────────────────────────

#[test]
fn with_model_overrides_and_none_keeps_the_provider_default() {
    let base = Executor::new(Arc::new(MockProvider::new("base-model")), Budget::new(4));
    assert_eq!(base.active_model(), "base-model");

    let overridden = base.with_model(Some("fast-model".into()));
    assert_eq!(overridden.active_model(), "fast-model");

    let cleared = base.with_model(None);
    assert_eq!(cleared.active_model(), "base-model");
}

// ── mvl wrappers write through to the session files ────────────────────────

fn read_events(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn every_mvl_wrapper_reaches_its_event() {
    let dir = tempfile::tempdir().unwrap();
    let mvl_path = dir.path().join("s.mvl.jsonl");
    let exe_path = dir.path().join("s.execution.jsonl");
    let session = Arc::new(MvlSession::open(&mvl_path, Some(&exe_path), "run-wrap").unwrap());
    let provider = Arc::new(MockProvider::new("mock"));
    let executor = Executor::new(provider, Budget::new(4)).with_mvl(session);

    executor.mvl_start(Some("the task"));
    let mut request = CompletionRequest::new(vec![Message {
        role: Role::User,
        content: "go".into(),
        tool_calls: vec![],
        tool_call_id: None,
    }]);
    request.tools.push(ToolDef {
        name: "read_file".into(),
        description: String::new(),
        parameters: json!({}),
    });
    executor.mvl_request(2, &request); // turn 2 → recorded as 1
    executor.mvl_completion(2, &CompletionResponse::text("partial answer"));
    let call = liberado_provider::ToolInvocation {
        id: "c9".into(),
        name: "read_file".into(),
        arguments: "{}".into(),
    };
    executor.mvl_tool_started(3, &call);
    executor.mvl_tool_result(3, &call, true, "body");
    executor.mvl_end("failed", "turn budget exhausted");

    let events = read_events(&mvl_path);
    fn kind(v: &serde_json::Value) -> &str {
        v["type"].as_str().unwrap_or("")
    }
    assert!(
        events.iter().any(|e| kind(e) == "run_started"),
        "{events:?}"
    );
    let prompt = events.iter().find(|e| kind(e) == "prompt").expect("prompt");
    assert_eq!(
        prompt["turn"], 1,
        "turn is 1-based via saturating_sub: {prompt}"
    );
    assert!(events.iter().any(|e| kind(e) == "completion"), "{events:?}");
    assert!(
        events.iter().any(|e| kind(e) == "tool_result"),
        "{events:?}"
    );
    let ended = events.iter().find(|e| kind(e) == "run_ended").expect("end");
    assert_eq!(ended["outcome"], "failed");

    let exe_events = read_events(&exe_path);
    assert!(
        exe_events.iter().any(|e| e["type"] == "tool_started"),
        "{exe_events:?}"
    );
}

// ── exhaustion step arithmetic ──────────────────────────────────────────────

#[tokio::test]
async fn the_wrap_up_reserve_sets_max_turns_to_turn_plus_two() {
    let provider = Arc::new(MockProvider::new("mock"));
    let executor = Executor::new(provider, Budget::new(30));
    let policy = RunPolicy {
        salvageable: true,
        loop_profile: LoopProfile::semantic(),
    };
    let mut wrapping_up = false;
    // Already past the cap: the reserve fires on the step that notices it.
    let mut max_turns = 5_u32;
    let mut tools = vec![
        ToolDef {
            name: "read_file".into(),
            description: String::new(),
            parameters: json!({}),
        },
        ToolDef {
            name: SUBMIT_REPORT_TOOL.into(),
            description: String::new(),
            parameters: json!({}),
        },
    ];
    let mut messages: Vec<Message> = Vec::new();
    let usage = ResourceUsage {
        turns: 6,
        elapsed: std::time::Duration::ZERO,
        tokens: 0,
    };

    let exhausted = executor.exhaustion_step(
        6,
        &usage,
        Mode::Report,
        &policy,
        &mut wrapping_up,
        &mut max_turns,
        &mut tools,
        &mut messages,
    );
    assert_eq!(exhausted, None, "a salvageable run gets its reserve");
    assert!(wrapping_up);
    // turn + WRAP_UP_TURNS - 1 == 6 + 3 - 1
    assert_eq!(max_turns, 8, "{max_turns}");
    assert_eq!(
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec![SUBMIT_REPORT_TOOL],
        "everything but the finish tool is withdrawn"
    );
    assert!(messages.last().unwrap().content.contains("run out of"));
}

// ── read/write batches ──────────────────────────────────────────────────────

struct ScriptedRuntime {
    catalog_tools: Vec<ToolDef>,
    results: std::sync::Mutex<std::collections::VecDeque<String>>,
}

impl ScriptedRuntime {
    fn new(names: &[&str], results: Vec<String>) -> Self {
        Self {
            catalog_tools: names
                .iter()
                .map(|n| ToolDef {
                    name: (*n).into(),
                    description: String::new(),
                    parameters: json!({}),
                })
                .collect(),
            results: std::sync::Mutex::new(results.into_iter().collect()),
        }
    }
    fn pop(&self) -> String {
        self.results
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted result")
    }
}

#[async_trait]
impl ToolRuntime for ScriptedRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.catalog_tools.clone()
    }
    async fn invoke(&self, _: &ToolInvocation) -> Result<String, String> {
        Ok(self.pop())
    }
}

fn read_call(id: &str, args: serde_json::Value) -> ToolInvocation {
    ToolInvocation {
        id: id.into(),
        name: "read_file".into(),
        arguments: args,
    }
}

fn empty_usage_policy() -> RunPolicy {
    RunPolicy {
        salvageable: true,
        loop_profile: LoopProfile::exact(),
    }
}

/// Two identical reads count exactly one repeat; two distinct reads count none.
#[tokio::test]
async fn repeat_call_counting_sees_identity_not_mere_presence() {
    let provider = Arc::new(MockProvider::new("mock"));
    let executor = Executor::new(provider, Budget::new(4));
    let policy = empty_usage_policy();

    // Identical arguments twice.
    let runtime = ScriptedRuntime::new(&["read_file"], vec!["one".into(), "two".into()]);
    let a = read_call("c1", json!({"path": "same.md"}));
    let mut messages: Vec<Message> = Vec::new();
    let mut history: Vec<(String, serde_json::Value, String)> = Vec::new();
    let mut repeat_calls = 0usize;
    let mut doom_hit: Option<String> = None;
    let mut cycle_hit: Option<Vec<String>> = None;
    let reads = [&a, &a];
    executor
        .run_reads(
            1,
            &reads,
            &runtime,
            &mut messages,
            &mut history,
            &mut repeat_calls,
            &mut doom_hit,
            &mut cycle_hit,
            &policy,
        )
        .await;
    assert_eq!(
        repeat_calls, 1,
        "the second identical call repeats: {history:?}"
    );

    // Distinct arguments never count.
    let b1 = read_call("c2", json!({"path": "x.md"}));
    let b2 = read_call("c3", json!({"path": "y.md"}));
    let runtime = ScriptedRuntime::new(&["read_file"], vec!["x body".into(), "y body".into()]);
    let mut repeat_calls = 0usize;
    let mut history = Vec::new();
    let reads = [&b1, &b2];
    executor
        .run_reads(
            2,
            &reads,
            &runtime,
            &mut messages,
            &mut history,
            &mut repeat_calls,
            &mut doom_hit,
            &mut cycle_hit,
            &policy,
        )
        .await;
    assert_eq!(
        repeat_calls, 0,
        "distinct calls are not repeats even though names match: {history:?}"
    );
}

/// A successful read must be recorded as successful in the trace — an inverted
/// ok-flag tells the operator every tool errored.
#[tokio::test]
async fn read_results_record_ok_true_for_success() {
    let dir = tempfile::tempdir().unwrap();
    let mvl_path = dir.path().join("r.mvl.jsonl");
    let session = Arc::new(MvlSession::open(&mvl_path, None, "run-reads").unwrap());
    let provider = Arc::new(MockProvider::new("mock"));
    let executor = Executor::new(provider, Budget::new(4)).with_mvl(session);
    let runtime = ScriptedRuntime::new(&["read_file"], vec!["good body".into()]);
    let policy = empty_usage_policy();

    let c = read_call("c1", json!({"path": "a.md"}));
    let mut messages: Vec<Message> = Vec::new();
    let mut history = Vec::new();
    let mut repeat_calls = 0usize;
    let mut doom_hit = None;
    let mut cycle_hit = None;
    let reads = [&c];
    executor
        .run_reads(
            0,
            &reads,
            &runtime,
            &mut messages,
            &mut history,
            &mut repeat_calls,
            &mut doom_hit,
            &mut cycle_hit,
            &policy,
        )
        .await;

    let result = read_events(&mvl_path)
        .into_iter()
        .find(|e| e["type"] == "tool_result")
        .expect("tool_result recorded");
    assert_eq!(result["ok"], true, "{result}");
}

#[tokio::test]
async fn write_results_record_ok_true_and_repeats_are_counted() {
    let dir = tempfile::tempdir().unwrap();
    let mvl_path = dir.path().join("w.mvl.jsonl");
    let session = Arc::new(MvlSession::open(&mvl_path, None, "run-writes").unwrap());
    let provider = Arc::new(MockProvider::new("mock"));
    let executor = Executor::new(provider, Budget::new(4)).with_mvl(session);
    let runtime = ScriptedRuntime::new(&["write_file"], vec!["ok".into(), "ok".into()]);
    let policy = empty_usage_policy();

    let w = ToolInvocation {
        id: "w1".into(),
        name: "write_file".into(),
        arguments: json!({"path": "same.md", "content": "x"}),
    };
    let mut messages: Vec<Message> = Vec::new();
    let mut history = Vec::new();
    let mut repeat_calls = 0usize;
    let mut doom_hit = None;
    let mut cycle_hit = None;
    let writes = [&w, &w];
    executor
        .run_writes(
            1,
            &writes,
            &runtime,
            &mut messages,
            &mut history,
            &mut repeat_calls,
            &mut doom_hit,
            &mut cycle_hit,
            &policy,
        )
        .await;

    assert_eq!(repeat_calls, 1, "{history:?}");
    let result = read_events(&mvl_path)
        .into_iter()
        .find(|e| e["type"] == "tool_result")
        .expect("recorded");
    assert_eq!(result["ok"], true, "{result}");
}

// ── short-cycle detection ───────────────────────────────────────────────────

fn hist(steps: &[(&str, &str)]) -> Vec<(String, serde_json::Value, String)> {
    steps
        .iter()
        .map(|(name, path)| ((*name).to_string(), json!({"path": path}), String::new()))
        .collect()
}

#[test]
fn a_period_two_alternation_is_detected_with_its_tools() {
    let history = hist(&[
        ("read_file", "a.md"),
        ("search_text", "q"),
        ("read_file", "a.md"),
        ("search_text", "q"),
    ]);
    let cycling = detect_short_cycle(&history).expect("ABAB is a cycle");
    assert_eq!(cycling.len(), 2);
}

#[test]
fn a_mono_streak_is_never_a_short_cycle() {
    let history = hist(&[
        ("read_note", "a.md"),
        ("read_note", "a.md"),
        ("read_note", "a.md"),
        ("read_note", "a.md"),
    ]);
    assert_eq!(detect_short_cycle(&history), None, "AAAA is doom-loop turf");
}

#[test]
fn short_histories_cannot_form_a_cycle() {
    let history = hist(&[("read_file", "a.md"), ("search_text", "q")]);
    assert_eq!(detect_short_cycle(&history), None);
}

/// A period-3 alternation is caught with its tools listed in walk order.
#[test]
fn a_period_three_alternation_is_detected_in_order() {
    let history = hist(&[
        ("read_file", "a.md"),
        ("search_text", "q"),
        ("write_file", "b.md"),
        ("read_file", "a.md"),
        ("search_text", "q"),
        ("write_file", "b.md"),
    ]);
    let cycling = detect_short_cycle(&history).expect("ABCABC is a cycle");
    assert_eq!(
        cycling,
        vec!["read_file", "search_text", "write_file"],
        "{cycling:?}"
    );
}

/// Disjoint token sets cosine to exactly zero — NaN from a broken idf would
/// fail this equality.
#[test]
fn disjoint_arguments_similarity_is_exactly_zero() {
    use serde_json::json;
    let a = json!({"query": "alpha"});
    let b = json!({"other": "beta"});
    assert_eq!(args_similarity(&a, &b), 0.0);
}
