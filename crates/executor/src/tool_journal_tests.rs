//! Tool rows carry sizes, not payloads, on every runtime path the face loop uses.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_common::{Outcome, ToolCall};
use liberado_provider::{
    CompletionResponse, LatencyEvent, LatencyRecorder, Message, MockProvider, ToolCallEvent,
    ToolDef, ToolInvocation,
};

use super::super::*;

struct Capture(Mutex<Vec<ToolCallEvent>>);

impl LatencyRecorder for Capture {
    fn record(&self, _event: LatencyEvent) {}

    fn record_tool(&self, event: ToolCallEvent) {
        self.0.lock().expect("capture lock").push(event);
    }
}

impl Capture {
    fn events(&self) -> Vec<ToolCallEvent> {
        self.0.lock().expect("capture lock").clone()
    }
}

struct Reply {
    body: Result<String, String>,
    read_only: bool,
}

#[async_trait]
impl ToolRuntime for Reply {
    fn catalog(&self) -> Vec<ToolDef> {
        vec![ToolDef::new(
            "live_spending",
            "read",
            serde_json::json!({"type": "object"}),
        )]
    }

    fn is_read_only(&self, _tool_name: &str) -> bool {
        self.read_only
    }

    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        self.body.clone()
    }
}

struct Named;

#[async_trait]
impl ToolRuntime for Named {
    fn catalog(&self) -> Vec<ToolDef> {
        vec![
            ToolDef::new("live_spending", "read", serde_json::json!({})),
            ToolDef::new("upcoming", "read", serde_json::json!({})),
        ]
    }

    fn is_read_only(&self, _tool_name: &str) -> bool {
        true
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        Ok(match call.name.as_str() {
            "live_spending" => "abc".into(),
            "upcoming" => "abcdef".into(),
            _ => "x".into(),
        })
    }
}

fn journaled(script: Vec<CompletionResponse>) -> (Executor, Arc<Capture>) {
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let capture = Arc::new(Capture(Mutex::new(Vec::new())));
    let exec = Executor::new(provider, Budget::new(8)).with_tool_journal(capture.clone());
    (exec, capture)
}

fn blob(events: &[ToolCallEvent]) -> String {
    events
        .iter()
        .map(|event| serde_json::to_string(event).expect("event json"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_write_records_sizes_and_leaves_the_payload_out() {
    let args = serde_json::json!({"account": "SECRET-ACCT-991"});
    let (exec, capture) = journaled(vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "live_spending",
            args.clone(),
        )]),
        CompletionResponse::text("done"),
    ]);
    let mut messages = vec![Message::user("balance")];
    liberado_provider::latency::with_correlation(
        "sess-budget",
        exec.converse_messages(
            &Reply {
                body: Ok("SECRET-BAL-991".into()),
                read_only: false,
            },
            &mut messages,
        ),
    )
    .await
    .expect("turn");

    let events = capture.events();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.kind, "tool_call");
    assert_eq!(event.correlation, "sess-budget");
    assert_eq!(event.turn, 1);
    assert_eq!(event.tool, "live_spending");
    assert_eq!(
        event.args_bytes,
        serde_json::to_vec(&args).expect("args").len() as u64
    );
    assert_eq!(event.result_bytes, "SECRET-BAL-991".len() as u64);
    assert!(!event.is_error);
    assert!(event.duration_ms < 60_000);
    let text = blob(&events);
    assert!(!text.contains("SECRET-ACCT-991"), "{text}");
    assert!(!text.contains("SECRET-BAL-991"), "{text}");
}

#[tokio::test]
async fn a_runtime_error_sets_the_flag_and_hides_the_message() {
    let (exec, capture) = journaled(vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "live_spending",
            serde_json::json!({}),
        )]),
        CompletionResponse::text("done"),
    ]);
    let mut messages = vec![Message::user("balance")];
    exec.converse_messages(
        &Reply {
            body: Err("SECRET-ERR-77".into()),
            read_only: false,
        },
        &mut messages,
    )
    .await
    .expect("the model still answers");

    let events = capture.events();
    assert_eq!(events.len(), 1);
    assert!(events[0].is_error);
    assert_eq!(
        events[0].result_bytes,
        "tool error: SECRET-ERR-77".len() as u64
    );
    assert!(!blob(&events).contains("SECRET-ERR-77"));
}

#[tokio::test]
async fn parallel_reads_can_be_summed_by_tool_and_result_bytes() {
    let (exec, capture) = journaled(vec![
        CompletionResponse::tool_calls(vec![
            ToolInvocation::new("a", "live_spending", serde_json::json!({"n": 1})),
            ToolInvocation::new("b", "upcoming", serde_json::json!({"n": 2})),
        ]),
        CompletionResponse::text("done"),
    ]);
    let mut messages = vec![Message::user("week")];
    exec.converse_messages(&Named, &mut messages)
        .await
        .expect("turn");

    let events = capture.events();
    let mut totals = std::collections::HashMap::<String, u64>::new();
    for event in &events {
        *totals.entry(event.tool.clone()).or_default() += event.result_bytes;
    }
    assert_eq!(totals.get("live_spending"), Some(&3));
    assert_eq!(totals.get("upcoming"), Some(&6));
    assert_eq!(
        events.iter().map(|event| event.result_bytes).sum::<u64>(),
        9
    );
}

#[tokio::test]
async fn a_spilled_result_counts_the_raw_payload() {
    let payload = "SECRET-PAYLOAD-4421".repeat(20);
    let dir = tempfile::tempdir().expect("spill dir");
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        vec![
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c1",
                "live_spending",
                serde_json::json!({}),
            )]),
            CompletionResponse::text("done"),
        ],
    ));
    let capture = Arc::new(Capture(Mutex::new(Vec::new())));
    let exec = Executor::new(provider, Budget::new(8))
        .with_tool_journal(capture.clone())
        .with_spill_dir(dir.path().to_path_buf())
        .with_spill_max_bytes(32);
    let mut messages = vec![Message::user("dump")];
    exec.converse_messages(
        &Reply {
            body: Ok(payload.clone()),
            read_only: false,
        },
        &mut messages,
    )
    .await
    .expect("turn");

    let events = capture.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].result_bytes, payload.len() as u64);
    assert!(events[0].result_bytes > 32);
    assert!(!blob(&events).contains("SECRET-PAYLOAD-4421"));
}

#[tokio::test]
async fn the_streaming_path_records_the_same_row() {
    let (exec, capture) = journaled(vec![
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "live_spending",
            serde_json::json!({"q": "SECRET-STREAM-ARG"}),
        )]),
        CompletionResponse::text("done"),
    ]);
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let mut messages = vec![Message::user("now")];
    liberado_provider::latency::with_correlation(
        "sess-stream",
        exec.converse_stream(
            &Reply {
                body: Ok("SECRET-STREAM-88".into()),
                read_only: false,
            },
            &mut messages,
            &tx,
        ),
    )
    .await
    .expect("stream");

    let events = capture.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].correlation, "sess-stream");
    assert_eq!(events[0].turn, 1);
    assert_eq!(events[0].result_bytes, "SECRET-STREAM-88".len() as u64);
    assert!(!events[0].is_error);
    let text = blob(&events);
    assert!(!text.contains("SECRET-STREAM-ARG"), "{text}");
    assert!(!text.contains("SECRET-STREAM-88"), "{text}");
}

#[tokio::test]
async fn a_seed_call_is_turn_zero_and_submit_report_is_not_a_tool_row() {
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        vec![CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c",
            SUBMIT_REPORT_TOOL,
            serde_json::json!({
                "outcome": "succeeded",
                "summary": "filed",
                "artifacts": [],
            }),
        )])],
    ));
    let capture = Arc::new(Capture(Mutex::new(Vec::new())));
    let exec = Executor::new(provider, Budget::new(4)).with_tool_journal(capture.clone());
    let runtime = Reply {
        body: Ok("SECRET-SEED-1".into()),
        read_only: false,
    };
    let task = Task::new("worker", "add").with_seed(vec![ToolCall {
        tool: "list_loans".into(),
        args: serde_json::json!({"id": "SECRET-SEED-ARG"}),
    }]);
    let report = exec.execute(&runtime, task).await.expect("report");
    assert_eq!(report.outcome, Outcome::Succeeded);

    let events = capture.events();
    assert_eq!(events.len(), 1, "submit_report stays in-process");
    assert_eq!(events[0].turn, 0);
    assert_eq!(events[0].tool, "list_loans");
    assert_eq!(events[0].result_bytes, "SECRET-SEED-1".len() as u64);
    let text = blob(&events);
    assert!(!text.contains("SECRET-SEED-ARG"), "{text}");
    assert!(!text.contains("SECRET-SEED-1"), "{text}");
}

#[tokio::test]
async fn no_journal_still_returns_the_answer() {
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        vec![CompletionResponse::text("plain")],
    ));
    let exec = Executor::new(provider, Budget::new(4));
    let mut messages = vec![Message::user("hi")];
    let answer = exec
        .converse_messages(
            &Reply {
                body: Ok("unused".into()),
                read_only: false,
            },
            &mut messages,
        )
        .await
        .expect("turn");
    assert_eq!(answer, "plain");
}
