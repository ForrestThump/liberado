use super::*;
use crate::loop_guard::{ARG_SIMILARITY_THRESHOLD, args_similarity};
use async_trait::async_trait;
use liberado_provider::{CompletionResponse, MockProvider};
use liberado_test_support::InvocationRecordingRuntime;
use std::sync::Mutex;

fn call_tool(name: &str) -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new("c", name, serde_json::json!({}))])
}

fn call_tool_with(name: &str, args: serde_json::Value) -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new("c", name, args)])
}

fn submit(args: serde_json::Value) -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new("c", SUBMIT_REPORT_TOOL, args)])
}

fn valid_report_args() -> serde_json::Value {
    serde_json::json!({
        "outcome": "succeeded",
        "summary": "found it",
        "artifacts": ["notes/answer.md"],
    })
}

fn executor(script: Vec<CompletionResponse>, budget: Budget) -> (Arc<MockProvider>, Executor) {
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let exec = Executor::new(provider.clone(), budget);
    (provider, exec)
}

/// A provider that lets `step` of wall-clock elapse on every call, so a budget can be exhausted
/// *during* a run rather than before it starts.
///
/// Needed because a test can only advance the clock before `execute` is entered, and
/// `run_started` is captured inside — so pre-advancing moves the start too and elapsed stays
/// zero. Time has to pass where the work happens, which is the provider call.
struct SlowProvider {
    inner: Arc<MockProvider>,
    step: std::time::Duration,
}

#[async_trait]
impl Provider for SlowProvider {
    fn model(&self) -> String {
        self.inner.model()
    }
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, liberado_provider::ProviderError> {
        liberado_common::clock::test_advance(self.step);
        self.inner.complete(request).await
    }
}

fn offered_tools(provider: &MockProvider) -> Vec<String> {
    provider
        .received_requests()
        .first()
        .map(|r| r.tools.iter().map(|t| t.name.clone()).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn runs_tools_across_turns_then_files_report() {
    let (provider, exec) = executor(
        vec![call_tool("search"), submit(valid_report_args())],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));

    let report = exec
        .execute(&runtime, Task::new("you are a worker", "find the thing"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(report.summary, "found it");
    assert_eq!(report.artifacts, vec!["notes/answer.md"]);
    // The real tool ran once; submit_report is handled by the engine, not the runtime.
    assert_eq!(runtime.invoked().len(), 1);
    assert_eq!(runtime.invoked()[0].name, "search");
    // The finish-tool was offered alongside the real catalog.
    let offered = offered_tools(&provider);
    assert!(offered.contains(&SUBMIT_REPORT_TOOL.to_string()));
    assert!(offered.contains(&"search".to_string()));
}

#[tokio::test]
async fn conversational_loop_ends_on_prose_without_finish_tool() {
    let (provider, exec) = executor(
        vec![
            call_tool("search"),
            CompletionResponse::text("the answer is 42"),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let answer = exec
        .converse(
            &runtime,
            Task::new("you are a helpful assistant", "what is it?"),
        )
        .await
        .unwrap();

    assert_eq!(answer, "the answer is 42");
    // Conversational mode must NOT inject the finish-tool — its consumer is a human.
    assert!(!offered_tools(&provider).contains(&SUBMIT_REPORT_TOOL.to_string()));
}

#[tokio::test]
async fn stream_emits_tool_started_and_finished_around_the_call() {
    let (_provider, exec) = executor(
        vec![call_tool("search"), CompletionResponse::text("found it")],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let mut messages = vec![
        Message::system("you are a helpful assistant"),
        Message::user("find the thing"),
    ];
    exec.converse_stream(&runtime, &mut messages, &tx)
        .await
        .unwrap();
    drop(tx);

    let mut events = Vec::new();
    while let Some(e) = rx.recv().await {
        events.push(e);
    }

    // The tool's start and outcome both surface, in order, bracketing the run.
    let started = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolStarted { name, .. } if name == "search"));
    let finished = events.iter().position(
        |e| matches!(e, AgentEvent::ToolFinished { name, ok, .. } if name == "search" && *ok),
    );
    let (started, finished) = (started.unwrap(), finished.unwrap());
    assert!(started < finished, "ToolStarted must precede ToolFinished");

    // The result preview rode along on the finish event.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolFinished { preview, .. } if preview == "3 hits"
    )));
    // The prose answer streamed as tokens after the tool.
    let answer: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Token(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(answer, "found it");
}

#[tokio::test]
async fn stream_marks_a_failed_tool_call_not_ok() {
    let (_provider, exec) = executor(
        vec![call_tool("search"), CompletionResponse::text("recovered")],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Err("boom".into()));
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let mut messages = vec![Message::system("sys"), Message::user("go")];
    exec.converse_stream(&runtime, &mut messages, &tx)
        .await
        .unwrap();
    drop(tx);

    let mut saw_failed = false;
    while let Some(e) = rx.recv().await {
        if let AgentEvent::ToolFinished { ok, preview, .. } = e {
            assert!(!ok, "a failed invoke must report ok=false");
            assert!(preview.contains("boom"));
            saw_failed = true;
        }
    }
    assert!(
        saw_failed,
        "expected a ToolFinished event for the failed call"
    );
}

#[tokio::test]
async fn budget_exhaustion_with_no_progress_is_a_failed_report() {
    // Every call errors — genuinely no progress — so exhaustion must stay `Failed`, not
    // `PartiallySucceeded`.
    let (_provider, exec) = executor(
        vec![call_tool("search"), call_tool("search")],
        Budget::new(2),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Err("upstream 500".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "loop forever"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Failed);
    assert!(report.summary.contains("budget"));
}

/// The live failure this exists for: a research subagent spent every turn searching
/// successfully and was cut off before it ever filed, so a run that had done the work returned
/// nothing but "ran out of turns". Salvageable work now gets turns back to write it up.
#[tokio::test]
async fn salvageable_work_gets_a_reserve_to_file_what_it_has() {
    let (_provider, exec) = executor(
        vec![
            call_tool("search"),
            call_tool("search"),
            // The reserve turn: only `submit_report` is still offered.
            submit(serde_json::json!({
                "outcome": "partially_succeeded",
                "summary": "found 3 of 4 themes",
                "artifacts": [],
            })),
        ],
        Budget::new(2),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));

    let report = exec
        .execute(
            &runtime,
            Task::new("worker", "research everything").salvageable(true),
        )
        .await
        .unwrap();

    // The model's own report, not the synthesized budget one.
    assert_eq!(report.outcome, Outcome::PartiallySucceeded);
    assert_eq!(report.summary, "found 3 of 4 themes");
}

/// The reserve is for *filing*, not for more work. Everything else is withdrawn when it is
/// granted, so a model that tries to keep going has nothing to call.
#[tokio::test]
async fn the_reserve_withdraws_every_tool_but_submit_report() {
    let (_provider, exec) = executor(
        vec![
            call_tool("search"),
            // Tries to keep searching after the reserve is granted.
            call_tool("search"),
            submit(serde_json::json!({
                "outcome": "partially_succeeded",
                "summary": "wrapped up",
                "artifacts": [],
            })),
        ],
        Budget::new(1),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));

    let report = exec
        .execute(
            &runtime,
            Task::new("worker", "research everything").salvageable(true),
        )
        .await
        .unwrap();

    assert_eq!(report.summary, "wrapped up");
    assert_eq!(
        runtime.invoked().len(),
        1,
        "only the pre-reserve search should have run; the reserve must not buy more work"
    );
}

/// All-or-nothing work is unchanged: a half-applied change is not partial credit, so there is
/// no reserve and the run fails exactly as it did before.
#[tokio::test]
async fn unsalvageable_work_gets_no_reserve() {
    let (_provider, exec) = executor(
        vec![
            call_tool("apply_patch"),
            call_tool("apply_patch"),
            submit(valid_report_args()),
        ],
        Budget::new(2),
    );
    let runtime = InvocationRecordingRuntime::new(&["apply_patch"], Ok("applied".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "refactor the module"))
        .await
        .unwrap();

    // Synthesized budget report, never reaching the scripted submit.
    assert!(report.summary.contains("budget"), "{}", report.summary);
    assert_eq!(runtime.invoked().len(), 2);
}

/// The reserve is granted once. A model that burns it without filing still gets the
/// synthesized report rather than looping on fresh grants.
#[tokio::test]
async fn the_reserve_is_granted_only_once() {
    // Never files; more scripted turns than budget + reserve so the loop would keep going if
    // the grant repeated.
    let script: Vec<_> = std::iter::repeat_with(|| call_tool("search"))
        .take(12)
        .collect();
    let (_provider, exec) = executor(script, Budget::new(2));
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));

    let report = exec
        .execute(
            &runtime,
            Task::new("worker", "research everything").salvageable(true),
        )
        .await
        .unwrap();

    assert!(report.summary.contains("budget"), "{}", report.summary);
    assert!(
        runtime.invoked().len() <= 2,
        "the reserve offers no other tool, so no further searches can land"
    );
}

#[tokio::test]
async fn budget_exhaustion_with_real_progress_is_partially_succeeded_and_names_the_calls() {
    // Two tool turns, never files; budget of 2 forces termination — but both calls actually
    // succeeded, so the deploying agent should see `PartiallySucceeded` and a summary naming
    // what happened, not a bare "ran out of turns" indistinguishable from zero progress.
    let (_provider, exec) = executor(
        vec![call_tool("search"), call_tool("search")],
        Budget::new(2),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "loop forever"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::PartiallySucceeded);
    assert!(report.summary.contains("budget"), "{}", report.summary);
    assert!(report.summary.contains("search"), "{}", report.summary);
    assert!(report.summary.contains("3 hits"), "{}", report.summary);
}

struct RefuseAll;

#[async_trait]
impl ReportGate for RefuseAll {
    async fn accept(&self, _report: &Report, _wrapping_up: bool) -> Result<(), String> {
        Err("NOT accepted — check is red".into())
    }
}

struct RefuseFirstSucceeded {
    remaining: std::sync::Mutex<u32>,
}

impl RefuseFirstSucceeded {
    fn once() -> Self {
        Self {
            remaining: std::sync::Mutex::new(1),
        }
    }
}

#[async_trait]
impl ReportGate for RefuseFirstSucceeded {
    async fn accept(&self, _report: &Report, _wrapping_up: bool) -> Result<(), String> {
        let mut left = self.remaining.lock().expect("gate mutex");
        if *left > 0 {
            *left -= 1;
            return Err("NOT accepted — check is red".into());
        }
        Ok(())
    }
}

/// A red same-session check refuses `succeeded` and keeps the conversation. The next
/// `succeeded` (once the check would pass) is accepted. Nothing about the work is reverted —
/// the gate only talks; it does not touch the worktree.
#[tokio::test]
async fn a_red_report_gate_refuses_succeeded_and_the_retry_is_accepted() {
    let (provider, exec) = executor(
        vec![submit(valid_report_args()), submit(valid_report_args())],
        Budget::default(),
    );
    let exec = exec.with_report_gate(Arc::new(RefuseFirstSucceeded::once()));
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .expect("the second succeeded must be accepted");

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(report.summary, "found it");
    assert_eq!(
        provider.received_requests().len(),
        2,
        "the first succeeded must be refused so the model gets another turn; a skipped gate would accept on turn 1"
    );
}

struct RefuseInfrastructure;

#[async_trait]
impl ReportGate for RefuseInfrastructure {
    async fn accept(&self, _report: &Report, _wrapping_up: bool) -> Result<(), String> {
        Err("FAILURE_CLASS: infrastructure\nREPAIR_HINT: stop\nno space on device".into())
    }
}

#[tokio::test]
async fn an_infrastructure_gate_refusal_ends_as_failed_without_asking_the_model() {
    let (provider, exec) = executor(vec![submit(valid_report_args())], Budget::default());
    let exec = exec.with_report_gate(Arc::new(RefuseInfrastructure));
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .expect("host failure must end the loop");

    assert_eq!(report.outcome, Outcome::Failed);
    assert!(
        report
            .summary
            .to_ascii_lowercase()
            .contains("infrastructure"),
        "{}",
        report.summary
    );
    assert_eq!(
        provider.received_requests().len(),
        1,
        "the model must not get another turn to 'fix' a full disk"
    );
}

/// Partial is already honest. A gate that would refuse everything must not be asked, or a
/// turn-budget wrap-up would trap the model with no tools and throw away the only report of
/// the work it did keep.
#[tokio::test]
async fn a_refuse_all_gate_still_accepts_partial() {
    let (_provider, exec) = executor(
        vec![submit(serde_json::json!({
            "outcome": "partially_succeeded",
            "summary": "half done, files stay",
        }))],
        Budget::default(),
    );
    let exec = exec.with_report_gate(Arc::new(RefuseAll));
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .expect("partial must end the loop even when the gate would refuse succeeded");

    assert_eq!(report.outcome, Outcome::PartiallySucceeded);
    assert_eq!(report.summary, "half done, files stay");
}

/// Wrap-up has already withdrawn every tool but `submit_report`. Refusing `succeeded` then
/// would leave the model unable to fix the check and unable to leave. The files stay either
/// way; we accept the report so the half-finished work is not reported as nothing.
#[tokio::test]
async fn wrap_up_succeeded_is_accepted_even_when_the_gate_would_refuse() {
    let (_provider, exec) = executor(
        vec![call_tool("search"), submit(valid_report_args())],
        Budget::new(1),
    );
    let exec = exec.with_report_gate(Arc::new(RefuseAll));
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));

    let report = exec
        .execute(
            &runtime,
            Task::new("worker", "research everything").salvageable(true),
        )
        .await
        .expect("wrap-up must accept the report so the work is not thrown away");

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(report.summary, "found it");
}

#[test]
fn report_ends_without_gate_keeps_honest_terminals() {
    assert!(Executor::report_ends_without_gate(
        Outcome::PartiallySucceeded,
        false
    ));
    assert!(Executor::report_ends_without_gate(Outcome::Failed, false));
    assert!(Executor::report_ends_without_gate(Outcome::Proposed, false));
    assert!(Executor::report_ends_without_gate(Outcome::Succeeded, true));
    assert!(!Executor::report_ends_without_gate(
        Outcome::Succeeded,
        false
    ));
}

/// A schema slip is correctable, so the run continues instead of throwing the work away.
///
/// Live failure this encodes: a coding run reached turn 12, called `submit_report` with
/// `outcome` missing, and the whole run aborted — every edit and every read discarded over one
/// absent field.
#[tokio::test]
async fn malformed_submit_report_is_handed_back_and_the_retry_is_accepted() {
    let (_provider, exec) = executor(
        vec![
            // Missing the required `summary` field.
            submit(serde_json::json!({ "outcome": "succeeded" })),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .expect("a corrected report should be accepted");

    assert_eq!(report.outcome, Outcome::Succeeded);
}

/// The retry is bounded — a model that never produces the shape still terminates the run.
#[tokio::test]
async fn repeatedly_malformed_submit_report_args_is_a_decode_error() {
    let malformed = || submit(serde_json::json!({ "outcome": "succeeded" }));
    let (_provider, exec) = executor(
        // One more than MAX_MALFORMED_REPORTS, so the last one is fatal.
        vec![malformed(), malformed(), malformed()],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let err = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap_err();

    assert!(matches!(err, ExecError::Decode(_)), "got {err:?}");
}

#[tokio::test]
async fn seed_calls_run_before_the_first_model_turn() {
    let (provider, exec) = executor(vec![submit(valid_report_args())], Budget::default());
    let runtime = InvocationRecordingRuntime::new(&["tasks-mcp:add"], Ok("added".into()));

    let task = Task::new("worker", "add a task").with_seed(vec![ToolCall {
        tool: "tasks-mcp:add".into(),
        args: serde_json::json!({ "title": "milk" }),
    }]);
    let report = exec.execute(&runtime, task).await.unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    // The seed executed against the runtime...
    assert_eq!(runtime.invoked().len(), 1);
    assert_eq!(runtime.invoked()[0].name, "tasks-mcp:add");
    // ...and the model's first turn already saw the seed's assistant call + tool result.
    let first = &provider.received_requests()[0].messages;
    assert!(first.iter().any(|m| m.role == Role::Tool));
    assert!(first.iter().any(|m| !m.tool_calls.is_empty()));
}

#[tokio::test]
async fn report_mode_nudges_once_then_wraps_prose() {
    let (provider, exec) = executor(
        vec![
            CompletionResponse::text("I think I'm done"),
            CompletionResponse::text("still just talking"),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    // Prose wrapped, not lost.
    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(report.summary, "still just talking");
    // The nudge was injected before the second turn.
    let second = &provider.received_requests()[1].messages;
    assert!(second.iter().any(|m| m.content == REPORT_NUDGE));
}

#[tokio::test]
async fn tool_failure_is_fed_back_in_band_not_aborted() {
    // First turn the tool errors; the loop must continue and let the model file anyway.
    let (_provider, exec) = executor(
        vec![call_tool("search"), submit(valid_report_args())],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Err("upstream 500".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    // The run completed (no abort) and the tool was still invoked.
    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(runtime.invoked().len(), 1);
}

const REAL_EDIT_1: &str = r#"{"path":"crates/acp-bridge/src/main.rs","old":"        assert_eq!(result[\\\"protocolVersion\\\"], PROTOCOL_VERSION);\n        assert_eq!(result[\\\"agentInfo\\\"][\\\"name\\\"], \\\"Liberado\\\");\n        // Must stay false until durable load+replay (P3); true lied to Paseo's resume path.\n        assert_eq!(result[\\\"agentCapabilities\\\"][\\\"loadSession\\\"], false);","new":"        assert_eq!(result[\\\"protocolVersion\\\"], PROTOCOL_VERSION);\n        assert_eq!(result[\\\"agentInfo\\\"][\\\"name\\\"], \\\"Liberado\\\");\n        assert_eq!(\n            result[\\\"agentCapabilities\\\"][\\\"loadSession\\\"],\n            LOAD_SESSION_CAPABILITY,\n            \\\"initialize must reflect LOAD_SESSION_CAPABILITY exactly\\\"\n        );"}"#;
const REAL_EDIT_2: &str = r#"{"path":"crates/acp-bridge/src/main.rs","new":"    #[tokio::test]\n    async fn load_then_prompt_appends_to_existing_transcript() {\n        let dir = tempfile::TempDir::new().unwrap();\n        let _guards = with_session_dir(&dir);\n\n        let sid = \"lib-append-after-load\";\n        session_store::save(&session_store::SessionRecord {\n            id: sid.to_string(),\n            mode: \"coding\".to_string(),\n            cwd: std::path::PathBuf::from(\"/tmp/proj\"),\n            model: \"m1\".to_string(),\n            messages: vec![\n                session_store::StoredMessage {\n                    role: \"user\".into(),\n                    content: \"initial question\".into(),\n                },\n                session_store::StoredMessage {\n                    role: \"assistant\".into(),\n                    content: \"initial answer\".into(),\n                },\n            ],\n            updated_at: \"2025-01-01T00:00:00Z\".into(),\n        })\n        .expect(\"save\");\n\n        let bridge = test_bridge();\n        let sink = CaptureSink {\n            lines: std::sync::Mutex::new(Vec::new()),\n        };\n        let _result = handle_request(\n            bridge.clone(),\n            &sink,\n            \"session/load\",\n            json!({ \"sessionId\": sid }),\n        )\n        .await\n        .expect(\"load must succeed\");\n\n        // Verify the session is registered in-memory with the right mode/cwd.\n        {\n            let sessions = bridge.acp_sessions.lock().await;\n            let sess = sessions\n                .get(sid)\n                .expect(\"session must be registered after load\");\n            assert_eq!(sess.mode, AgentMode::Coding);\n            assert_eq!(sess.cwd, std::path::PathBuf::from(\"/tmp/proj\"));\n        }\n\n        // Simulate what run_session_prompt does after a turn: persist new messages.\n        session_store::append_messages(sid, \"new question\", \"new answer\")\n            .expect(\"append must succeed\");\n\n        let loaded = session_store::load(sid)\n            .expect(\"load\")\n            .expect(\"record must be present\");\n\n        assert_eq!(loaded.messages.len(), 4);\n        assert_eq!(loaded.messages[0].content, \"initial question\");\n        assert_eq!(loaded.messages[1].content, \"initial answer\");\n        assert_eq!(loaded.messages[2].content, \"new question\");\n        assert_eq!(loaded.messages[3].content, \"new answer\");\n    }","old":"    #[tokio::test]\n    async fn load_then_prompt_appends_to_existing_transcript() {\n        let dir = tempfile::TempDir::new().unwrap();\n        let _guards = with_session_dir(&dir);\n\n        let sid = \"lib-append-after-load\";\n        session_store::save(&session_store::SessionRecord {\n            id: sid.to_string(),\n            mode: \"coding\".to_string(),\n            cwd: std::path::PathBuf::from(\"/tmp/proj\"),\n            model: \"m1\".to_string(),\n            messages: vec![\n                session_store::StoredMessage {\n                    role: \"user\".into(),\n                    content: \"initial question\".into(),\n                },\n                session_store::StoredMessage {\n                    role: \"assistant\".into(),\n                    content: \"initial answer\".into(),\n                },\n            ],\n            updated_at: \"2025-01-01T00:00:00Z\".into(),\n        })\n        .expect(\"save\");\n\n        let bridge = test_bridge();\n        let sink = CaptureSink {\n            lines: std::sync::Mutex::new(Vec::new()),\n        };\n        let _result = handle_request(\n            bridge,\n            &sink,\n            \"session/load\",\n            json!({ \"sessionId\": sid }),\n        )\n        .await\n        .expect(\"load must succeed\");\n\n        // Now append messages (simulating what run_session_prompt does after a prompt).\n        session_store::append_messages(sid, \"new question\", \"new answer\")\n            .expect(\"append must succeed\");\n\n        let loaded = session_store::load(sid)\n            .expect(\"load\")\n            .expect(\"record must be present\");\n\n        assert_eq!(loaded.messages.len(), 4);\n        assert_eq!(loaded.messages[0].content, \"initial question\");\n        assert_eq!(loaded.messages[1].content, \"initial answer\");\n        assert_eq!(loaded.messages[2].content, \"new question\");\n        assert_eq!(loaded.messages[3].content, \"new answer\");\n    }"}"#;
const REAL_EDIT_3: &str = r#"{"new":"    }\n}","old":"    }\n\n    #[test]\n    fn initialize_advertises_load_session_capability() {\n        // Must be true once load is implemented.\n        assert!(\n            LOAD_SESSION_CAPABILITY,\n            \"initialize must advertise loadSession:true now that session/load restores history\"\n        );\n    }\n}","path":"crates/acp-bridge/src/main.rs"}"#;

/// The three consecutive `edit_file` calls that actually got the tool withdrawn, verbatim from
/// `coder-traces/lib-18ca9815159fee44-22288-attempt-2`. Recorded arguments, not a reconstruction:
/// a synthetic pair I wrote by hand did *not* reproduce the failure, which is how I learned the
/// hand-written version was testing nothing.
#[test]
fn the_real_recorded_edits_are_not_a_doom_loop() {
    let hist: Vec<(String, serde_json::Value, String)> = [REAL_EDIT_1, REAL_EDIT_2, REAL_EDIT_3]
        .iter()
        .map(|a| {
            (
                "edit_file".to_string(),
                serde_json::from_str(a).expect("recorded args must parse"),
                "ok".to_string(),
            )
        })
        .collect();
    assert!(
        !is_doom_loop(&hist, LoopProfile::semantic()),
        "three real, different edits must not read as thrash"
    );
}

/// An edit tool used repeatedly on one file is a change being applied, not a loop.
///
/// Measured on 2026-08-11: four consecutive `edit_file` calls — different anchors, two files —
/// got `edit_file` withdrawn on the next turn, and the run ended with the model naming two
/// broken call sites it no longer had a tool to fix. The arguments of an edit *are* file
/// content, so two different edits to one file always score as near-duplicates.
/// The guard must still fire on the pathology it exists for: the *same* edit, resent.
/// Replaying a byte-identical edit accomplishes nothing however many times it is sent.
#[tokio::test]
async fn the_identical_edit_repeated_is_still_a_doom_loop() {
    let same = || {
        call_tool_with(
            "edit_file",
            serde_json::json!({"path": "src/main.rs", "old": "a", "new": "b"}),
        )
    };
    let (provider, exec) = executor(
        vec![
            same(),
            same(),
            same(),
            call_tool("read_file"),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["edit_file", "read_file"], Ok("done".into()));

    let _ = exec
        .execute(&runtime, Task::new("worker", "apply the change"))
        .await
        .unwrap();

    assert!(
        provider
            .received_requests()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.content == DOOM_LOOP_NUDGE)),
        "an identical edit resent three times must still trip the guard"
    );
}

#[tokio::test]
async fn doom_loop_is_nudged_once_then_recovers_on_a_different_call() {
    // Three identical "search" calls trip the guard; a nudge is injected instead of a 4th
    // identical call being allowed through, and the model diversifying afterward still
    // completes normally.
    let (provider, exec) = executor(
        vec![
            call_tool("search"),
            call_tool("search"),
            call_tool("search"),
            call_tool("other_tool"),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime =
        InvocationRecordingRuntime::new(&["search", "other_tool"], Ok("same result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    // All 4 real tool calls ran (the guard nudges, it doesn't skip execution).
    assert_eq!(runtime.invoked().len(), 4);
    // The nudge was injected as its own message right after the 3rd identical call.
    assert!(
        provider
            .received_requests()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.content == DOOM_LOOP_NUDGE)),
        "expected the doom-loop nudge to have been sent to the model"
    );
}

#[tokio::test]
async fn doom_loop_persisting_past_the_nudge_removes_the_tool_then_aborts_if_it_still_repeats() {
    // Escalation ladder: 1st detection (3rd identical call) nudges; 2nd (4th) removes the tool
    // from what's offered and explains why; 3rd (5th — the tool somehow got called again
    // anyway) refuses that call and lets the run continue, so the model can still finish with
    // the tools it has left. Ending the run here used to discard everything it had already
    // done: a live coding run had edited ten files across six crates when it re-read one file
    // once too often, and the abort threw the whole attempt away before it could verify.
    let (provider, exec) = executor(
        vec![
            call_tool("search"),
            call_tool("search"),
            call_tool("search"),
            call_tool("search"),
            call_tool("search"),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("same result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    // The run survived the third strike and filed the model's own report.
    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(runtime.invoked().len(), 5);
    assert!(
        provider
            .received_requests()
            .iter()
            .any(|r| r.messages.iter().any(|m| m
                .content
                .contains("every further call to it will be refused"))),
        "expected the model to be told the tool is withdrawn, not silently cut off"
    );
    // The tool was actually removed from the offered catalog, not just talked about — checked
    // on the final request, sent after removal.
    assert!(
        provider
            .received_requests()
            .last()
            .unwrap()
            .tools
            .iter()
            .all(|t| t.name != "search"),
        "expected `search` to be gone from the offered tools by the final turn"
    );
    assert!(
        provider.received_requests().iter().any(|r| r
            .messages
            .iter()
            .any(|m| m.content.contains("removed for the rest of this task"))),
        "expected the tool-removal explanation to have been sent"
    );
}

#[tokio::test]
async fn doom_loop_tool_removal_lets_the_task_actually_succeed() {
    // The point of removing the tool instead of just failing: the model can still finish using
    // what it already has, once the repeated tool is no longer an option.
    let (_provider, exec) = executor(
        vec![
            call_tool("search"),
            call_tool("search"),
            call_tool("search"),         // 1st detection: nudged
            call_tool("search"),         // 2nd detection: `search` removed
            submit(valid_report_args()), // no longer able to repeat `search` -> finishes instead
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("same result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(runtime.invoked().len(), 4);
}

#[tokio::test]
async fn recovery_bonus_rescues_a_tight_budget_where_removal_would_otherwise_arrive_too_late() {
    // Regression for a real live finding: `ExecuteDirect`'s actual 4-turn budget means the
    // nudge (turn 3) and tool removal (turn 4) land on the very last nominal turn — with no
    // bonus, removal would be immediately followed by budget exhaustion, never able to pay off.
    // A budget of 4 (mirroring `liberado_orchestrator::DIRECT_MAX_TURNS`), needing a 5th turn
    // (only reachable via the one-time bonus) to actually finish.
    let (_provider, exec) = executor(
        vec![
            call_tool("search"),
            call_tool("search"),
            call_tool("search"),         // turn 3: 1st detection, nudged
            call_tool("search"),         // turn 4: 2nd detection, `search` removed + bonus granted
            submit(valid_report_args()), // turn 5: only reachable because of the bonus
        ],
        Budget::new(4),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("same result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(
        report.outcome,
        Outcome::Succeeded,
        "without the bonus this would be Failed (budget exhausted at turn 4): {report:?}"
    );
    assert_eq!(runtime.invoked().len(), 4);
}

#[tokio::test]
async fn doom_loop_is_detected_within_a_single_turn_of_parallel_calls() {
    // A model can request several tool calls in one turn (parallel calling) — 3 identical
    // calls batched into a single response must trip the guard just like 3 across turns: the
    // nudge fires after the 3rd of the batch (skipping any further calls in that same batch)
    // and the loop moves straight to the next model turn, which here recovers cleanly.
    let parallel_search = CompletionResponse::tool_calls(vec![
        ToolInvocation::new("a", "search", serde_json::json!({})),
        ToolInvocation::new("b", "search", serde_json::json!({})),
        ToolInvocation::new("c", "search", serde_json::json!({})),
    ]);
    let (provider, exec) = executor(
        vec![parallel_search, submit(valid_report_args())],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("same result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    // Exactly the 3 batched calls ran — the guard didn't need a 4th to fire within one turn.
    assert_eq!(runtime.invoked().len(), 3);
    assert!(
        provider
            .received_requests()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.content == DOOM_LOOP_NUDGE)),
        "expected the doom-loop nudge to have been sent after the 3rd batched call"
    );
}

fn call_tool_with_args(name: &str, args: serde_json::Value) -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new("c", name, args)])
}

#[tokio::test]
async fn doom_loop_catches_a_rephrased_repeat_not_just_a_byte_identical_one() {
    // Regression for the real live finding: DeepSeek rewording the same question each call
    // ("turbomcp transport layer" -> "turbo-mcp transport Provider trait stdio HTTP..." -> ...)
    // defeated a byte-equality check entirely. These three are lifted from that transcript.
    let (provider, exec) = executor(
        vec![
            call_tool_with_args(
                "deepwiki",
                serde_json::json!({ "query": "turbomcp transport layer" }),
            ),
            call_tool_with_args(
                "deepwiki",
                serde_json::json!({ "query": "turbo-mcp transport layer implementation Provider trait stdio HTTP" }),
            ),
            call_tool_with_args(
                "deepwiki",
                serde_json::json!({ "query": "turbomcp transport Provider trait stdio HTTP JSON-RPC MCP protocol" }),
            ),
            call_tool_with_args("vault", serde_json::json!({ "note": "summary" })),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(
        &["deepwiki", "vault"],
        Ok("turbomcp uses stdio and HTTP transports.".into()),
    );

    let report = exec
        .execute(&runtime, Task::new("worker", "research and save"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    let invoked = runtime.invoked();
    assert_eq!(invoked.len(), 4);
    assert_eq!(invoked[3].name, "vault");
    // The decisive check: the guard actually fired after the 3rd rephrased call. Without it,
    // this test would pass for the wrong reason — the script reaches `vault` at turn 4 either
    // way, since it's simply next in the scripted sequence.
    assert!(
        provider
            .received_requests()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.content == DOOM_LOOP_NUDGE)),
        "expected the doom-loop nudge to have fired for the 3 rephrased deepwiki calls"
    );
}

#[tokio::test]
async fn distinct_queries_to_the_same_tool_are_not_flagged_as_a_doom_loop() {
    // The false-positive case: genuinely different queries to the same tool, back to back,
    // must NOT trip the guard just because the tool name repeats.
    let (provider, exec) = executor(
        vec![
            call_tool_with_args(
                "search",
                serde_json::json!({ "query": "weather in Denver" }),
            ),
            call_tool_with_args(
                "search",
                serde_json::json!({ "query": "capital of France" }),
            ),
            call_tool_with_args(
                "search",
                serde_json::json!({ "query": "current bitcoin price" }),
            ),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("a result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "look up three things"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    // All 3 distinct queries ran, uninterrupted by any nudge.
    assert_eq!(runtime.invoked().len(), 3);
    assert!(
        !provider
            .received_requests()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.content == DOOM_LOOP_NUDGE)),
        "distinct queries to the same tool must not trip the doom-loop guard"
    );
}

#[tokio::test]
async fn short_cycle_between_two_tools_is_nudged_then_recovers() {
    // A,B,A,B is a different failure shape than one tool repeating — same guard family
    // (VTCode's `detect_patterns`), exact tool-name match, no argument comparison needed.
    let (provider, exec) = executor(
        vec![
            call_tool("tool-a"),
            call_tool("tool-b"),
            call_tool("tool-a"),
            call_tool("tool-b"),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["tool-a", "tool-b"], Ok("result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(runtime.invoked().len(), 4);
    assert!(
        provider
            .received_requests()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.content == CYCLE_NUDGE)),
        "expected the cycle nudge to have been sent"
    );
}

// ------------------------------------------------------------------
// Loop-guard pure unit tests (hard-coded call histories).
// Short cycle = multi-tool name thrash. Doom loop = same tool + similar args.
// ------------------------------------------------------------------

fn hist(calls: &[(&str, serde_json::Value)]) -> Vec<(String, serde_json::Value, String)> {
    calls
        .iter()
        .map(|(name, args)| ((*name).into(), args.clone(), "ok".into()))
        .collect()
}

#[test]
fn mono_tool_parallel_batch_is_not_a_short_cycle() {
    // Dogfood 01KX7BWV: five parallel read_note calls must not match period-2 as AAAA.
    let h = hist(&[
        (
            "turbovault:read_note",
            serde_json::json!({"path": "Tasks/a.md"}),
        ),
        (
            "turbovault:read_note",
            serde_json::json!({"path": "Tasks/b.md"}),
        ),
        (
            "turbovault:read_note",
            serde_json::json!({"path": "Tasks/c.md"}),
        ),
        (
            "turbovault:read_note",
            serde_json::json!({"path": "Tasks/d.md"}),
        ),
        (
            "turbovault:read_note",
            serde_json::json!({"path": "Tasks/e.md"}),
        ),
    ]);
    assert!(
        detect_short_cycle(&h).is_none(),
        "same tool repeated is doom-loop territory, not short-cycle"
    );
}

#[test]
fn different_path_read_notes_are_not_a_doom_loop() {
    // Legitimate multi-file read: same tool, different path args — not thrash.
    let h = hist(&[
        (
            "turbovault:read_note",
            serde_json::json!({"path": "Tasks/Sarah.md"}),
        ),
        (
            "turbovault:read_note",
            serde_json::json!({"path": "Life/Relationships/Weekly.md"}),
        ),
        (
            "turbovault:read_note",
            serde_json::json!({"path": "Work/RTX Onboarding.md"}),
        ),
        (
            "turbovault:read_note",
            serde_json::json!({"path": "House/Chores.md"}),
        ),
        (
            "turbovault:read_note",
            serde_json::json!({"path": "Projects/Homelab.md"}),
        ),
    ]);
    assert!(
        !is_doom_loop(&h, LoopProfile::semantic()),
        "distinct paths must not look like near-duplicate args"
    );
    assert!(detect_short_cycle(&h).is_none());
    // Pairwise similarity should sit below the doom threshold (calibration guardrail).
    for window in h.windows(2) {
        let sim = args_similarity(&window[0].1, &window[1].1);
        assert!(
            sim < ARG_SIMILARITY_THRESHOLD,
            "path pair sim {sim} should be < {ARG_SIMILARITY_THRESHOLD}: {:?} vs {:?}",
            window[0].1,
            window[1].1
        );
    }
}

/// The live regression: a research subagent was stopped three times for "near-duplicate
/// arguments" while issuing genuinely different search queries. Bag-of-words scores them as
/// similar because they share the topic vocabulary — which is exactly what varied queries on
/// one subject look like.
#[test]
fn varied_search_queries_trip_the_semantic_profile_but_not_the_exact_one() {
    let h = hist(&[
        (
            "search:search_web",
            serde_json::json!({"query": "agentic AI orchestration anti-patterns"}),
        ),
        (
            "search:search_web",
            serde_json::json!({"query": "agentic AI orchestration failure modes"}),
        ),
        (
            "search:search_web",
            serde_json::json!({"query": "agentic AI orchestration token waste"}),
        ),
    ]);

    assert!(
        is_doom_loop(&h, LoopProfile::semantic()),
        "precondition: this is the false positive the exact profile exists to avoid"
    );
    assert!(
        !is_doom_loop(&h, LoopProfile::exact()),
        "distinct queries are the work, not a loop"
    );
}

/// Relaxing the bar must not disable the guard: re-running the *same* query is still thrash.
#[test]
fn the_exact_profile_still_catches_a_literally_repeated_call() {
    let q = serde_json::json!({"query": "agentic AI orchestration"});
    let h = hist(&[
        ("search:search_web", q.clone()),
        ("search:search_web", q.clone()),
        ("search:search_web", q),
    ]);
    assert!(is_doom_loop(&h, LoopProfile::exact()));
}

#[test]
fn semantic_is_the_default_profile() {
    assert_eq!(LoopProfile::default().arg_match, ArgMatch::Semantic);
}

#[test]
fn same_path_read_note_three_times_is_a_doom_loop() {
    // Mono-tool thrash: same tool + same args — doom-loop's job, not short-cycle.
    let path = serde_json::json!({"path": "Tasks/Sarah.md"});
    let h = hist(&[
        ("turbovault:read_note", path.clone()),
        ("turbovault:read_note", path.clone()),
        ("turbovault:read_note", path),
    ]);
    assert!(
        is_doom_loop(&h, LoopProfile::semantic()),
        "identical path ×3 must trip doom-loop"
    );
    assert!(
        detect_short_cycle(&h).is_none(),
        "mono-tool must not also be classified as short-cycle"
    );
}

#[test]
fn empty_args_same_tool_three_times_is_a_doom_loop() {
    let empty = serde_json::json!({});
    let h = hist(&[
        ("search", empty.clone()),
        ("search", empty.clone()),
        ("search", empty),
    ]);
    assert!(is_doom_loop(&h, LoopProfile::semantic()));
    assert!(detect_short_cycle(&h).is_none());
}

#[test]
fn abab_pattern_is_still_a_short_cycle() {
    let h = hist(&[
        ("tool-a", serde_json::json!({})),
        ("tool-b", serde_json::json!({})),
        ("tool-a", serde_json::json!({})),
        ("tool-b", serde_json::json!({})),
    ]);
    let cycling = detect_short_cycle(&h).expect("A,B,A,B should cycle");
    assert_eq!(cycling, vec!["tool-a".to_string(), "tool-b".to_string()]);
    // Multi-tool name thrash is not a mono-tool doom loop.
    assert!(!is_doom_loop(&h, LoopProfile::semantic()));
}

#[test]
fn abcabc_period_three_is_a_short_cycle() {
    let h = hist(&[
        ("a", serde_json::json!({})),
        ("b", serde_json::json!({})),
        ("c", serde_json::json!({})),
        ("a", serde_json::json!({})),
        ("b", serde_json::json!({})),
        ("c", serde_json::json!({})),
    ]);
    let cycling = detect_short_cycle(&h).expect("A,B,C,A,B,C should cycle");
    assert_eq!(
        cycling,
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
}

/// Alternating read/search over *different* targets is exploration, not a cycle.
///
/// Verbatim from the coding run this guard broke: the model was wiring `inbox_ignore_globs`
/// across crates, the names alternated `read_file`/`search_text`, and a names-only detector
/// flagged it on the 4th call and removed both tools for the rest of the task. Every call here
/// names a distinct file or query, so nothing is being repeated.
#[test]
fn alternating_reads_over_distinct_targets_are_not_a_cycle() {
    let h = hist(&[
        (
            "read_file",
            serde_json::json!({"path": "crates/config-loader/src/model/tuning.rs"}),
        ),
        ("search_text", serde_json::json!({"query": "CaptureTuning"})),
        (
            "read_file",
            serde_json::json!({"path": "crates/daemon/src/vault_source.rs"}),
        ),
        (
            "search_text",
            serde_json::json!({"query": "inbox_ignore_globs"}),
        ),
    ]);
    assert!(
        detect_short_cycle(&h).is_none(),
        "distinct targets each turn is progress, not thrash"
    );
    assert!(!is_doom_loop(&h, LoopProfile::semantic()));
}

/// Same-file read → edit → reread → edit is the mandated coding loop.
/// Semantic path-identity would treat both edits as the same action;
/// Exact args do not, because `old`/`new` changed.
#[test]
fn read_then_edit_the_same_file_is_not_a_cycle() {
    let h = hist(&[
        (
            "read_file",
            serde_json::json!({"path": "src/lib.rs", "start_line": 10}),
        ),
        (
            "edit_file",
            serde_json::json!({"path": "src/lib.rs", "old": "fn a()", "new": "fn b()"}),
        ),
        (
            "read_file",
            serde_json::json!({"path": "src/lib.rs", "start_line": 10}),
        ),
        (
            "edit_file",
            serde_json::json!({"path": "src/lib.rs", "old": "fn b()", "new": "fn c()"}),
        ),
    ]);
    assert!(
        detect_short_cycle(&h).is_none(),
        "the mandated read-edit-reread-edit loop must not withdraw edit_file"
    );
}

/// Replaying the same edit is a cycle. A blanket skip of any cycle that
/// contains a mutating tool would miss this.
#[test]
fn identical_read_then_edit_replay_is_a_cycle() {
    let read = serde_json::json!({"path": "src/lib.rs", "start_line": 10});
    let edit = serde_json::json!({"path": "src/lib.rs", "old": "fn a()", "new": "fn b()"});
    let h = hist(&[
        ("read_file", read.clone()),
        ("edit_file", edit.clone()),
        ("read_file", read),
        ("edit_file", edit),
    ]);
    let cycling = detect_short_cycle(&h).expect("replaying the same edit is a cycle");
    assert_eq!(
        cycling,
        vec!["edit_file".to_string(), "read_file".to_string()]
    );
}

#[test]
fn edit_write_with_changing_content_is_not_a_cycle() {
    let h = hist(&[
        (
            "edit_file",
            serde_json::json!({"path": "a.rs", "old": "fn a()", "new": "fn b()"}),
        ),
        (
            "write_file",
            serde_json::json!({"path": "b.rs", "contents": "one"}),
        ),
        (
            "edit_file",
            serde_json::json!({"path": "a.rs", "old": "fn b()", "new": "fn c()"}),
        ),
        (
            "write_file",
            serde_json::json!({"path": "b.rs", "contents": "two"}),
        ),
    ]);
    assert!(
        detect_short_cycle(&h).is_none(),
        "different edit/write bodies are progress"
    );
}

#[test]
fn edit_write_with_identical_content_is_a_cycle() {
    let edit = serde_json::json!({"path": "a.rs", "old": "fn a()", "new": "fn b()"});
    let write = serde_json::json!({"path": "b.rs", "contents": "one"});
    let h = hist(&[
        ("edit_file", edit.clone()),
        ("write_file", write.clone()),
        ("edit_file", edit),
        ("write_file", write),
    ]);
    let cycling = detect_short_cycle(&h).expect("identical edit/write replay is a cycle");
    assert_eq!(
        cycling,
        vec!["edit_file".to_string(), "write_file".to_string()]
    );
}

#[test]
fn three_identical_run_command_calls_are_still_a_doom_loop() {
    let args = serde_json::json!({"program": "rg", "args": ["fn catalog", "lib.rs"]});
    let h = hist(&[
        ("run_command", args.clone()),
        ("run_command", args.clone()),
        ("run_command", args),
    ]);
    assert!(
        is_doom_loop(&h, LoopProfile::semantic()),
        "replaying the same command is still thrash"
    );
}

#[test]
fn three_different_run_command_searches_are_not_a_doom_loop() {
    let h = hist(&[
        (
            "run_command",
            serde_json::json!({"program": "rg", "args": ["fn catalog", "lib.rs"]}),
        ),
        (
            "run_command",
            serde_json::json!({"program": "rg", "args": ["fn git_commit", "lib.rs"]}),
        ),
        (
            "run_command",
            serde_json::json!({"program": "rg", "args": ["CommandPolicy", "lib.rs"]}),
        ),
    ]);
    assert!(
        !is_doom_loop(&h, LoopProfile::semantic()),
        "distinct searches under run_command are not a doom loop"
    );
}

#[test]
fn alternating_reads_over_identical_targets_are_a_cycle() {
    let h = hist(&[
        ("read_file", serde_json::json!({"path": "a.rs"})),
        ("search_text", serde_json::json!({"query": "needle"})),
        ("read_file", serde_json::json!({"path": "a.rs"})),
        ("search_text", serde_json::json!({"query": "needle"})),
    ]);
    let cycling = detect_short_cycle(&h).expect("same tools on the same targets is a cycle");
    assert_eq!(
        cycling,
        vec!["read_file".to_string(), "search_text".to_string()]
    );
}

/// Partial progress still counts as progress: one slot repeats, the other advances.
///
/// The queries here are deliberately realistic identifiers rather than toy words. Two very
/// short strings under a shared key (`{"query":"first"}` vs `{"query":"second"}`) score 0.26 —
/// above [`ARG_SIMILARITY_THRESHOLD`] — because the shared `query` token carries most of the
/// weight when there is almost no other text to compare. Real search terms of ordinary length
/// separate cleanly (this pair scores 0.16).
#[test]
fn cycle_requires_every_slot_to_repeat_not_just_one() {
    let h = hist(&[
        ("read_file", serde_json::json!({"path": "same.rs"})),
        ("search_text", serde_json::json!({"query": "CaptureTuning"})),
        ("read_file", serde_json::json!({"path": "same.rs"})),
        (
            "search_text",
            serde_json::json!({"query": "inbox_ignore_globs"}),
        ),
    ]);
    assert!(
        detect_short_cycle(&h).is_none(),
        "a re-read paired with a new search is still moving forward"
    );
}

#[test]
fn two_identical_calls_are_not_yet_a_doom_loop() {
    // Threshold is 3 — two repeats is allowed (batch of two different intents might
    // still share a tool; wait for the third near-duplicate).
    let path = serde_json::json!({"path": "Tasks/Sarah.md"});
    let h = hist(&[
        ("turbovault:read_note", path.clone()),
        ("turbovault:read_note", path),
    ]);
    assert!(!is_doom_loop(&h, LoopProfile::semantic()));
}

#[test]
fn rephrased_deepwiki_queries_still_count_as_near_duplicates() {
    // Keep the live-calibration cluster that justified ARG_SIMILARITY_THRESHOLD.
    let h = hist(&[
        (
            "deepwiki",
            serde_json::json!({ "query": "turbomcp transport layer" }),
        ),
        (
            "deepwiki",
            serde_json::json!({
                "query": "turbo-mcp transport layer implementation Provider trait stdio HTTP"
            }),
        ),
        (
            "deepwiki",
            serde_json::json!({
                "query": "turbomcp transport Provider trait stdio HTTP JSON-RPC MCP protocol"
            }),
        ),
    ]);
    assert!(
        is_doom_loop(&h, LoopProfile::semantic()),
        "rephrased same question must still trip doom-loop"
    );
}

#[test]
fn identity_path_mismatch_forces_zero_similarity() {
    let a = serde_json::json!({"path": "Tasks/A.md"});
    let b = serde_json::json!({"path": "Tasks/B.md"});
    assert_eq!(args_similarity(&a, &b), 0.0);
    assert_eq!(args_similarity(&a, &a), 1.0);
}

#[tokio::test]
async fn parallel_tool_batch_always_answers_every_tool_call_id_before_cycle_nudge() {
    // Dogfood D3: a turn with multiple tool_calls must produce one tool-result per id
    // before any cycle nudge / next provider call, or OpenAI-compat returns HTTP 400.
    let parallel = CompletionResponse::tool_calls(vec![
        ToolInvocation::new("c1", "tool-a", serde_json::json!({})),
        ToolInvocation::new("c2", "tool-b", serde_json::json!({})),
        ToolInvocation::new("c3", "tool-a", serde_json::json!({})),
        ToolInvocation::new("c4", "tool-b", serde_json::json!({})),
    ]);
    let (provider, exec) = executor(
        vec![parallel, submit(valid_report_args())],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["tool-a", "tool-b"], Ok("result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(runtime.invoked().len(), 4);
    // The request that follows the parallel batch must have tool results for c1..c4.
    let requests = provider.received_requests();
    assert!(requests.len() >= 2, "expected at least batch + follow-up");
    let follow_up = &requests[1];
    let tool_ids: Vec<_> = follow_up
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    for id in ["c1", "c2", "c3", "c4"] {
        assert!(
            tool_ids.contains(&id),
            "missing tool result for {id} in follow-up messages; got {tool_ids:?}"
        );
    }
}

#[tokio::test]
async fn parallel_different_path_reads_do_not_trip_cycle_or_doom() {
    // Dogfood 01KX7BWV shape: one turn with several read_note calls to *different* files.
    // Must complete all tools, then continue without cycle/doom nudges.
    let parallel = CompletionResponse::tool_calls(vec![
        ToolInvocation::new(
            "r1",
            "turbovault:read_note",
            serde_json::json!({"path": "Tasks/Sarah.md"}),
        ),
        ToolInvocation::new(
            "r2",
            "turbovault:read_note",
            serde_json::json!({"path": "Life/Relationships/Weekly.md"}),
        ),
        ToolInvocation::new(
            "r3",
            "turbovault:read_note",
            serde_json::json!({"path": "Work/RTX Onboarding.md"}),
        ),
        ToolInvocation::new(
            "r4",
            "turbovault:read_note",
            serde_json::json!({"path": "House/Chores.md"}),
        ),
        ToolInvocation::new(
            "r5",
            "turbovault:read_note",
            serde_json::json!({"path": "Projects/Homelab.md"}),
        ),
    ]);
    let (provider, exec) = executor(
        vec![parallel, submit(valid_report_args())],
        Budget::default(),
    );
    let runtime =
        InvocationRecordingRuntime::new(&["turbovault:read_note"], Ok("note body".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "read several notes"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(runtime.invoked().len(), 5);
    assert!(
        !any_message_contains(&provider, CYCLE_NUDGE),
        "parallel multi-file reads must not trip short-cycle"
    );
    assert!(
        !any_message_contains(&provider, DOOM_LOOP_NUDGE),
        "distinct paths must not trip doom-loop"
    );
    // All five tool results present before the submit_report turn.
    let follow_up = &provider.received_requests()[1];
    for id in ["r1", "r2", "r3", "r4", "r5"] {
        assert!(
            follow_up
                .messages
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some(id)),
            "missing tool result for {id}"
        );
    }
}

#[tokio::test]
async fn same_path_read_three_times_trips_doom_not_cycle() {
    // Mono-tool thrash with identical args → doom-loop nudge, never short-cycle.
    let path = serde_json::json!({"path": "Tasks/Sarah.md"});
    let (provider, exec) = executor(
        vec![
            call_tool_with_args("turbovault:read_note", path.clone()),
            call_tool_with_args("turbovault:read_note", path.clone()),
            call_tool_with_args("turbovault:read_note", path),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime =
        InvocationRecordingRuntime::new(&["turbovault:read_note"], Ok("note body".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "read one note"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert!(
        any_message_contains(&provider, DOOM_LOOP_NUDGE),
        "same path ×3 must trip doom-loop"
    );
    assert!(
        !any_message_contains(&provider, CYCLE_NUDGE),
        "mono-tool thrash must not be reported as short-cycle"
    );
}

#[tokio::test]
async fn short_cycle_escalates_to_removing_both_cycling_tools_then_aborts_if_it_persists() {
    // Same three-strike ladder as the doom-loop guard: nudge (turn 4), remove both cycling
    // tools and explain why (turn 5), then — if it somehow still repeats — refuse the calls
    // and let the run continue rather than discarding whatever it has already accomplished.
    let (provider, exec) = executor(
        vec![
            call_tool("tool-a"),
            call_tool("tool-b"),
            call_tool("tool-a"),
            call_tool("tool-b"),
            call_tool("tool-a"),
            call_tool("tool-b"),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["tool-a", "tool-b"], Ok("result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert!(
        provider
            .received_requests()
            .last()
            .unwrap()
            .tools
            .iter()
            .all(|t| t.name != "tool-a" && t.name != "tool-b"),
        "expected both cycling tools to be gone from the offered tools by the final turn"
    );
}

#[tokio::test]
async fn short_cycle_tool_removal_lets_the_task_actually_succeed() {
    let (_provider, exec) = executor(
        vec![
            call_tool("tool-a"),
            call_tool("tool-b"),
            call_tool("tool-a"),
            call_tool("tool-b"),         // 1st detection: nudged
            call_tool("tool-a"),         // 2nd detection: tool-a and tool-b both removed
            submit(valid_report_args()), // no longer able to cycle -> finishes instead
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["tool-a", "tool-b"], Ok("result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(runtime.invoked().len(), 5);
}

#[tokio::test]
async fn a_doom_loop_gets_its_own_nudge_even_after_the_cycle_guard_already_struck_once() {
    // Regression for the shared-counter bug (`docs/future-work/archive/hygiene-audit-2026-07-05.md` P2.1):
    // the short-cycle guard strikes first (tool-a/tool-b alternating), then, entirely
    // unrelated, `search` repeats 3x in a row for the FIRST time. With one counter shared
    // between both mechanisms, doom-loop's first-ever detection would have inherited the
    // cycle guard's strike count and jumped straight to tool removal — never nudging for the
    // doom loop at all. With independent `LoopGuard`s, doom-loop's first detection must still
    // be a nudge.
    let (provider, exec) = executor(
        vec![
            call_tool("tool-a"),
            call_tool("tool-b"),
            call_tool("tool-a"),
            call_tool("tool-b"), // cycle guard: 1st detection -> nudged
            call_tool("filler"), // breaks the cycle tail pattern
            call_tool("search"),
            call_tool("search"),
            call_tool("search"), // doom guard: 1st-ever detection -> must also be nudged
            call_tool("other_tool"),
            submit(valid_report_args()),
        ],
        // 10 scripted turns exceed `DEFAULT_MAX_TURNS` (8), so this needs its own explicit budget.
        Budget::new(10),
    );
    let runtime = InvocationRecordingRuntime::new(
        &["tool-a", "tool-b", "filler", "search", "other_tool"],
        Ok("result".into()),
    );

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert_eq!(runtime.invoked().len(), 9);
    assert!(
        provider
            .received_requests()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.content == CYCLE_NUDGE)),
        "expected the cycle nudge to have fired first"
    );
    assert!(
        provider
            .received_requests()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.content == DOOM_LOOP_NUDGE)),
        "expected the doom-loop guard's own 1st-strike nudge, not a skip-straight-to-removal"
    );
    assert!(
        !provider.received_requests().iter().any(|r| r
            .messages
            .iter()
            .any(|m| m.content.contains("removed for the rest of this task"))),
        "neither guard should have escalated to removal — each only struck once"
    );
}

#[tokio::test]
async fn wall_clock_limit_exhausts_before_the_first_turn_when_set_to_zero() {
    // A zero-duration wall-clock limit is exhausted the instant any time at all has passed —
    // deterministic without needing a real sleep, and proves the check runs before the
    // provider is even called (no responses are consumed from the script).
    let (provider, exec) = executor(
        vec![submit(valid_report_args())],
        Budget::new(4).with_wall_clock(std::time::Duration::ZERO),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Failed);
    assert!(report.summary.contains("wall-clock"), "{}", report.summary);
    assert_eq!(
        provider.received_requests().len(),
        0,
        "the wall-clock check must fire before any provider call is made"
    );
}

/// Freeze the clock, advance by exactly the budget — pins the `>=` boundary at a *non-zero*
/// duration, which the `Duration::ZERO` test above cannot distinguish from "exhausted before
/// the first check".
///
/// Was ignored on the belief that the clock could not inject time here. The real cause was that
/// `usage.elapsed` used `Instant::elapsed()` — real time — while `run_started` came from the
/// injectable clock, so advancing it moved nothing. With both ends on one clock the test runs.
#[tokio::test]
async fn wall_clock_limit_exhausts_at_exact_non_zero_boundary() {
    let t0 = std::time::Instant::now();
    let clock = liberado_common::clock::test_freeze_at(t0);

    // One second elapses inside the provider call, so the budget is hit mid-run at exactly the
    // boundary — `>=`, not `>`.
    // A tool call first, so the run reaches a second turn: the budget is checked at the *top*
    // of each iteration, so turn 1 always sees zero elapsed. The second check sees exactly the
    // one second the provider consumed — the `>=` boundary this test exists to pin.
    let inner = Arc::new(MockProvider::with_script(
        "mock",
        vec![call_tool("search"), submit(valid_report_args())],
    ));
    let exec = Executor::new(
        Arc::new(SlowProvider {
            inner,
            step: std::time::Duration::from_secs(1),
        }),
        Budget::new(10).with_wall_clock(std::time::Duration::from_secs(1)),
    );
    let _ = &clock; // held for the run; thaws on drop

    // The call fails, so nothing is salvageable and the run ends `Failed` rather than
    // `PartiallySucceeded` — keeping this test on the plain exhaustion report, which is the one
    // that has to name the resource.
    let runtime = InvocationRecordingRuntime::new(&["search"], Err("boom".into()));
    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Failed);
    // The assertion the original test was written to make, and was ignored for.
    assert!(
        report.summary.contains("wall-clock"),
        "the report must name the bound that actually ran out, not blame turns: {}",
        report.summary
    );
    // `clock` thaws on drop here — including if an assertion above panics.
}

#[tokio::test]
async fn token_limit_exhausts_once_accumulated_usage_crosses_it() {
    fn tool_call_with_usage(id: &str, tokens: u32) -> CompletionResponse {
        CompletionResponse {
            content: None,
            tool_calls: vec![ToolInvocation::new(id, "search", serde_json::json!({}))],
            finish_reason: liberado_provider::FinishReason::ToolCalls,
            usage: Some(liberado_provider::Usage {
                prompt_tokens: tokens / 2,
                completion_tokens: tokens / 2,
                total_tokens: tokens,
                cached_prompt_tokens: None,
                reasoning_tokens: None,
            }),
        }
    }
    let script = vec![
        tool_call_with_usage("c1", 100),
        tool_call_with_usage("c2", 100),
        submit(valid_report_args()),
    ];
    let (_provider, exec) = executor(
        script,
        Budget::new(10).with_token_limit(150), // exhausted after the 2nd response (total 200)
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("a result".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::PartiallySucceeded);
    assert!(report.summary.contains("tokens"), "{}", report.summary);
    // Both search calls ran (200 tokens spent) before the 3rd turn's token check stopped it —
    // the 3rd scripted response (submit) was never reached.
    assert_eq!(runtime.invoked().len(), 2);
}

fn scratchpad_call(id: &str, items: serde_json::Value) -> ToolInvocation {
    ToolInvocation::new(id, SCRATCHPAD_TOOL, serde_json::json!({ "items": items }))
}

/// Whether any message sent to the provider across the whole run contains `needle` — used to
/// prove a nudge (doom-loop/cycle) never fired, without depending on internal escalation state.
fn any_message_contains(provider: &MockProvider, needle: &str) -> bool {
    provider
        .received_requests()
        .iter()
        .any(|req| req.messages.iter().any(|m| m.content.contains(needle)))
}

#[tokio::test]
async fn scratchpad_injected_in_report_mode_only() {
    let (provider, exec) = executor(vec![submit(valid_report_args())], Budget::default());
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));
    exec.execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();
    assert!(offered_tools(&provider).contains(&SCRATCHPAD_TOOL.to_string()));

    let (provider, exec) = executor(vec![CompletionResponse::text("done")], Budget::default());
    exec.converse(&runtime, Task::new("assistant", "hi"))
        .await
        .unwrap();
    assert!(!offered_tools(&provider).contains(&SCRATCHPAD_TOOL.to_string()));
}

#[tokio::test]
async fn scratchpad_call_handled_in_process_never_reaches_the_runtime() {
    let (_, exec) = executor(
        vec![
            CompletionResponse::tool_calls(vec![scratchpad_call(
                "c1",
                serde_json::json!([{"content": "step one", "status": "in_progress"}]),
            )]),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    // The scratchpad call never went through ToolRuntime at all.
    assert!(runtime.invoked().is_empty());
}

/// One `tool_result` per `tool_call_id`, even when the wrap-up reserve is running.
///
/// OpenAI-compat providers reject an assistant `tool_calls` message answered by two results
/// carrying the same id (dogfood D3, 01KX7AGD). A scratchpad call that arrives while
/// `wrapping_up` is set matches both the wrap-up refusal and the scratchpad handler, so this
/// pins that only one of them may answer it.
#[tokio::test]
async fn scratchpad_during_wrap_up_emits_exactly_one_tool_result() {
    let (provider, exec) = executor(
        vec![
            // Spends the 1-call budget, so the reserve is granted for the next turn.
            call_tool("search"),
            // Arrives while the reserve is running.
            CompletionResponse::tool_calls(vec![scratchpad_call(
                "sp-wrap",
                serde_json::json!([{"content": "step one", "status": "in_progress"}]),
            )]),
            submit(serde_json::json!({
                "outcome": "partially_succeeded",
                "summary": "wrapped up",
                "artifacts": [],
            })),
        ],
        Budget::new(1),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));

    exec.execute(
        &runtime,
        Task::new("worker", "research everything").salvageable(true),
    )
    .await
    .unwrap();

    let requests = provider.received_requests();
    let last = requests.last().expect("at least one request");
    let answers = last
        .messages
        .iter()
        .filter(|m| m.tool_call_id.as_deref() == Some("sp-wrap"))
        .count();
    assert_eq!(
        answers, 1,
        "exactly one tool_result may answer tool_call_id `sp-wrap`, got {answers}"
    );
}

#[tokio::test]
async fn scratchpad_result_is_fed_back_as_a_tool_result() {
    let (provider, exec) = executor(
        vec![
            CompletionResponse::tool_calls(vec![scratchpad_call(
                "sp-1",
                serde_json::json!([{"content": "step one", "status": "in_progress"}]),
            )]),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));
    exec.execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    // The 2nd request (sent after the scratchpad call) must include a tool-result message
    // correlated to "sp-1" with the scratchpad's own confirmation text.
    let requests = provider.received_requests();
    let second_request = &requests[1];
    let tool_result = second_request
        .messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("sp-1"))
        .expect("expected a tool-result message for the scratchpad call");
    assert!(
        tool_result.content.contains("in_progress"),
        "{}",
        tool_result.content
    );
}

#[tokio::test]
async fn three_consecutive_scratchpad_updates_do_not_trigger_the_doom_loop_guard() {
    // Near-identical args each time (same content, only the status token differs) — exactly
    // the shape that would trip `is_doom_loop`'s cosine-similarity check if scratchpad calls
    // were tracked in `call_history` like a real tool.
    let (provider, exec) = executor(
        vec![
            CompletionResponse::tool_calls(vec![scratchpad_call(
                "c1",
                serde_json::json!([{"content": "investigate the bug", "status": "todo"}]),
            )]),
            CompletionResponse::tool_calls(vec![scratchpad_call(
                "c2",
                serde_json::json!([{"content": "investigate the bug", "status": "in_progress"}]),
            )]),
            CompletionResponse::tool_calls(vec![scratchpad_call(
                "c3",
                serde_json::json!([{"content": "investigate the bug", "status": "done"}]),
            )]),
            call_tool("search"),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert!(!any_message_contains(&provider, DOOM_LOOP_NUDGE));
}

#[tokio::test]
async fn alternating_real_tool_and_scratchpad_does_not_trigger_the_cycle_guard() {
    // [real_tool, scratchpad_write, real_tool, scratchpad_write] is a textbook period-2 cycle
    // by tool NAME alone (which is all `detect_short_cycle` checks) — the exact "call a tool,
    // then record progress" pattern this guard must not punish.
    let (provider, exec) = executor(
        vec![
            call_tool("search"),
            CompletionResponse::tool_calls(vec![scratchpad_call(
                "c1",
                serde_json::json!([{"content": "step one", "status": "done"}]),
            )]),
            call_tool("search"),
            CompletionResponse::tool_calls(vec![scratchpad_call(
                "c2",
                serde_json::json!([{"content": "step one", "status": "done"}, {"content": "step two", "status": "in_progress"}]),
            )]),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Succeeded);
    assert!(!any_message_contains(&provider, CYCLE_NUDGE));
    assert_eq!(
        runtime.invoked().len(),
        2,
        "both real search calls should have run"
    );
}

// ------------------------------------------------------------------
// repeat-call counting (deliverable 3a)
// ------------------------------------------------------------------

/// A run with no repeated tool calls must report `repeat_calls: 0`.
#[tokio::test]
async fn zero_repeats_reported_when_no_tool_call_was_repeated() {
    let (_provider, exec) = executor(
        vec![call_tool("search"), submit(valid_report_args())],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));
    let report = exec
        .execute(&runtime, Task::new("you are a worker", "find the thing"))
        .await
        .unwrap();
    assert_eq!(report.repeat_calls, 0);
}

/// Two byte-identical calls to the same tool must increment `repeat_calls`.
#[tokio::test]
async fn an_exact_repeat_increments_the_repeat_calls_counter() {
    let (_provider, exec) = executor(
        vec![
            call_tool("search"),
            call_tool("search"),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));
    let report = exec
        .execute(&runtime, Task::new("you are a worker", "find the thing"))
        .await
        .unwrap();
    assert_eq!(
        report.repeat_calls, 1,
        "the second `search` call (empty args) is a byte-exact repeat of the first"
    );
}

/// Near-but-not-equal argument sets are **not** counted as repeats. Exact matching is the
/// whole point — fuzzy duplicate detection is the doom-loop guard's job.
#[tokio::test]
async fn nearly_equal_args_are_not_counted_as_repeats() {
    let (_provider, exec) = executor(
        vec![
            call_tool_with("search", serde_json::json!({"q": "hello"})),
            call_tool_with("search", serde_json::json!({"q": "world"})),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));
    let report = exec
        .execute(&runtime, Task::new("you are a worker", "find the thing"))
        .await
        .unwrap();
    assert_eq!(
        report.repeat_calls, 0,
        "different args are never an exact repeat — near-duplicate is the doom-loop guard's concern"
    );
}

/// Counting must not change execution behaviour: even a repeated call must still be *made*.
#[tokio::test]
async fn a_repeated_call_is_still_executed_not_deduplicated() {
    let (_provider, exec) = executor(
        vec![
            call_tool("search"),
            call_tool("search"),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));
    let report = exec
        .execute(&runtime, Task::new("you are a worker", "find the thing"))
        .await
        .unwrap();
    assert!(report.repeat_calls > 0, "the repeat must be counted");
    let invocations = runtime.invoked();
    assert_eq!(invocations.len(), 2, "both calls must have been made");
    assert_eq!(invocations[0].name, "search");
    assert_eq!(invocations[1].name, "search");
}

/// **The journal must sum to the same number the report files.**
///
/// `repeat_calls` on a `LatencyEvent` is journaled per completion and the cost rollup *sums*
/// it, so the value has to be each call's own share. Journaling the running total made a run
/// with 2 real repeats land as `[None, None, Some(1), Some(2)]` and roll up as **3**.
///
/// R7: the wrong implementation being excluded is exactly that running total. The existing
/// rollup test cannot see it — it constructs `LatencyEvent`s by hand and asserts they add up,
/// so the fixture encodes the summing assumption without ever running the executor. This drives
/// the real loop through a `MeteredProvider` and compares the journal against the report.
#[tokio::test]
async fn journaled_repeat_calls_sum_to_the_reported_total() {
    use liberado_provider::{AgentRole, LatencyEvent, LatencyRecorder, MeteredProvider};

    #[derive(Default)]
    struct Rec {
        events: std::sync::Mutex<Vec<LatencyEvent>>,
    }
    impl LatencyRecorder for Rec {
        fn record(&self, e: LatencyEvent) {
            self.events.lock().unwrap().push(e);
        }
    }

    let rec = Arc::new(Rec::default());
    // Three identical `search` calls: the 2nd and 3rd are repeats, so the truth is 2.
    let inner = Arc::new(MockProvider::with_script(
        "m",
        vec![
            call_tool("search"),
            call_tool("search"),
            call_tool("search"),
            submit(valid_report_args()),
        ],
    ));
    let exec = Executor::new(
        MeteredProvider::wrap(inner, AgentRole::Orchestrator, rec.clone()),
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("hits".into()));

    let report = exec
        .execute(&runtime, Task::new("sys", "goal"))
        .await
        .unwrap();

    assert_eq!(
        report.repeat_calls, 2,
        "precondition: the run really had 2 repeats"
    );
    let journaled: usize = rec
        .events
        .lock()
        .unwrap()
        .iter()
        .map(|e| e.repeat_calls.unwrap_or(0))
        .sum();
    assert_eq!(
        journaled, report.repeat_calls,
        "the journal must sum to what the report filed, or liberado-cost over-counts"
    );
}

/// A repeat that arrives in the **same batch as `submit_report`, after it**, must still be
/// counted.
///
/// `submit_report` is decoded early in the per-call loop — before the counting block runs for
/// the rest of the batch — so stamping `repeat_calls` onto the report at decode time files a
/// count short by every repeat that follows it in the same response. The count is stamped at
/// the return site instead, once the whole batch has been walked.
///
/// R7: the wrong implementation being excluded is `report.repeat_calls = repeat_calls` in the
/// decode arm, which passes every other repeat test in this module because they all put
/// `submit_report` in its own later response.
#[tokio::test]
async fn a_repeat_after_submit_report_in_the_same_batch_is_still_counted() {
    let batched = CompletionResponse::tool_calls(vec![
        ToolInvocation::new("c-submit", SUBMIT_REPORT_TOOL, valid_report_args()),
        ToolInvocation::new("c-dup", "search", serde_json::json!({})),
    ]);
    // First response makes the `search` call; the second repeats it *after* submit_report.
    let (_provider, exec) = executor(vec![call_tool("search"), batched], Budget::default());
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("3 hits".into()));

    let report = exec
        .execute(&runtime, Task::new("you are a worker", "find the thing"))
        .await
        .unwrap();

    assert_eq!(
        report.repeat_calls, 1,
        "the repeat trailing submit_report in the same batch must be counted"
    );
}

// ── parallel read-only execution ──────────────────────────────────

struct ReadOnlyAwareRuntime {
    inner: InvocationRecordingRuntime,
    read_only_tools: Vec<String>,
}

#[async_trait]
impl ToolRuntime for ReadOnlyAwareRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        self.inner.catalog()
    }
    fn is_read_only(&self, tool_name: &str) -> bool {
        self.read_only_tools.contains(&tool_name.to_string())
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.inner.invoke(call).await
    }
}

/// Concurrent reads still answer each `tool_call_id` exactly once, in the order the model
/// asked — the results are zipped back by position, not looked up by id.
///
/// The ids here are distinct so a mismatch is visible; `read_a` and `read_b` also carry
/// different arguments so a swapped result would show up as the wrong content.
#[tokio::test]
async fn parallel_read_retains_tool_invocation_order_in_request() {
    let (provider, exec) = executor(
        vec![
            CompletionResponse::tool_calls(vec![
                ToolInvocation::new("read_a", "read_file", serde_json::json!({"path": "a.txt"})),
                ToolInvocation::new("read_b", "search_text", serde_json::json!({"query": "x"})),
            ]),
            submit(valid_report_args()),
        ],
        Budget::default(),
    );
    let runtime = ReadOnlyAwareRuntime {
        inner: InvocationRecordingRuntime::new(&["read_file", "search_text"], Ok("data".into())),
        read_only_tools: vec!["read_file".into(), "search_text".into()],
    };

    let report = exec
        .execute(&runtime, Task::new("worker", "do the thing"))
        .await
        .unwrap();
    assert_eq!(report.outcome, Outcome::Succeeded);
    let invoked = runtime.inner.invoked();
    assert_eq!(invoked.len(), 2);

    // The follow-up request must answer both calls, once each, in the asked order.
    let requests = provider.received_requests();
    let answered: Vec<&str> = requests[1]
        .messages
        .iter()
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    assert_eq!(
        answered,
        vec!["read_a", "read_b"],
        "one tool_result per id, in tool_calls order"
    );
}

// ── converse_messages ────────────────────────────────────────────

#[tokio::test]
async fn converse_messages_returns_final_prose() {
    let (_provider, exec) = executor(
        vec![
            call_tool("search"),
            CompletionResponse::text("the answer is 42"),
        ],
        Budget::default(),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));

    let mut messages = vec![
        Message::system("you are a helpful assistant"),
        Message::user("what is it?"),
    ];
    let answer = exec
        .converse_messages(&runtime, &mut messages)
        .await
        .unwrap();
    assert_eq!(answer, "the answer is 42");
    assert_eq!(messages.len(), 5);
}

#[path = "conversation_reserve_tests.rs"]
mod conversation_reserve_tests;

// ── converse_stream budget exhaustion ────────────────────────────

#[tokio::test]
async fn converse_stream_errors_on_budget_exhaustion() {
    let (_provider, exec) = executor(
        vec![call_tool("search"), call_tool("search")],
        Budget::new(1),
    );
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("data".into()));
    let (tx, _rx) = tokio::sync::mpsc::channel(64);

    let mut messages = vec![Message::system("helper"), Message::user("find")];
    let err = exec
        .converse_stream(&runtime, &mut messages, &tx)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("BudgetExceeded") || msg.contains("turns"),
        "got: {msg}"
    );
}

struct ParkOnAsk {
    inner: InvocationRecordingRuntime,
}

#[async_trait]
impl ToolRuntime for ParkOnAsk {
    fn catalog(&self) -> Vec<ToolDef> {
        self.inner.catalog()
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.inner.invoke(call).await
    }
    fn parks_for_human(&self, name: &str) -> bool {
        name == "ask_human"
    }
}

#[tokio::test]
async fn converse_stream_parks_without_a_tool_result() {
    let (_provider, exec) = executor(vec![call_tool("ask_human")], Budget::default());
    let runtime = ParkOnAsk {
        inner: InvocationRecordingRuntime::new(&["ask_human"], Ok("which crate?".into())),
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let mut messages = vec![Message::system("sys"), Message::user("split this")];
    let err = exec
        .converse_stream(&runtime, &mut messages, &tx)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ExecError::AwaitingHuman { ref call_id } if call_id == "c"),
        "got {err:?}"
    );
    assert_eq!(messages.len(), 3, "no tool result until the human answers");
    assert_eq!(messages[2].role, Role::Assistant);
    assert!(
        !messages.iter().any(|m| m.role == Role::Tool),
        "parking must not invent a tool result"
    );
}

#[tokio::test]
async fn converse_stream_resumes_after_the_human_answer() {
    let (_provider, exec) = executor(
        vec![
            call_tool("ask_human"),
            CompletionResponse::text("ok, crate A"),
        ],
        Budget::default(),
    );
    let runtime = ParkOnAsk {
        inner: InvocationRecordingRuntime::new(&["ask_human"], Ok("which crate?".into())),
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let mut messages = vec![Message::system("sys"), Message::user("split this")];
    let err = exec
        .converse_stream(&runtime, &mut messages, &tx)
        .await
        .unwrap_err();
    let ExecError::AwaitingHuman { call_id } = err else {
        panic!("expected park, got {err:?}");
    };
    messages.push(Message::tool_result(call_id, "crate A"));
    exec.converse_stream(&runtime, &mut messages, &tx)
        .await
        .unwrap();
    assert_eq!(
        messages.last().map(|m| m.content.as_str()),
        Some("ok, crate A")
    );
}

#[derive(Default)]
struct Recorder {
    turns: Mutex<Vec<TurnRecord>>,
    requests: Mutex<Vec<RequestRecord>>,
}

impl TurnObserver for Recorder {
    fn on_turn(&self, record: TurnRecord) {
        self.turns.lock().expect("recorder poisoned").push(record);
    }

    fn on_request(&self, record: RequestRecord) {
        self.requests
            .lock()
            .expect("recorder poisoned")
            .push(record);
    }
}

/// The loop must actually call `on_request`, not merely be able to.
///
/// The unit tests around the tracer prove the record is turned into an event correctly; they
/// cannot see whether anything ever produces one. Deleting the call site left every one of
/// them green and was caught only by a dead-code warning, which is a thin thread to hang the
/// one feature that tells us what the model was told.
#[tokio::test]
async fn the_loop_reports_every_request_before_making_it() {
    let rec = Arc::new(Recorder::default());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        vec![
            CompletionResponse::text("thinking"),
            submit(valid_report_args()),
        ],
    ));
    let exec = Executor::new(provider, Budget::default()).with_observer(rec.clone());
    let runtime = InvocationRecordingRuntime::new(&["search", "write_file"], Ok("data".into()));

    let _ = exec.execute(&runtime, Task::new("worker", "do it")).await;

    let requests = rec.requests.lock().unwrap().clone();
    let turns = rec.turns.lock().unwrap().clone();
    assert!(
        !requests.is_empty(),
        "no request was ever reported; the trace cannot say what the model was sent"
    );
    assert_eq!(
        requests.len(),
        turns.len(),
        "one request per turn: {} requests, {} turns",
        requests.len(),
        turns.len()
    );
    assert!(
        requests[0].tools_offered.contains(&"search".to_string()),
        "the request must record what the model could reach: {:?}",
        requests[0].tools_offered
    );
    assert!(
        !requests[0].system_prompt_sha256.is_empty(),
        "every request must be hashed so a mid-run prompt change is visible"
    );
}

/// The observer must answer, without reading any source, the three questions that cost the
/// most time debugging real runs: what could the model reach, what did it say, and why did the
/// turn end.
#[tokio::test]
async fn a_turn_record_answers_what_was_offered_what_was_said_and_why_it_ended() {
    let rec = Arc::new(Recorder::default());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        vec![
            CompletionResponse::text("Let me look at the config first."),
            submit(valid_report_args()),
        ],
    ));
    let exec = Executor::new(provider, Budget::default()).with_observer(rec.clone());
    let runtime = InvocationRecordingRuntime::new(&["search", "write_file"], Ok("data".into()));

    let _ = exec.execute(&runtime, Task::new("worker", "do it")).await;

    let turns = rec.turns.lock().unwrap().clone();
    assert!(
        turns.len() >= 2,
        "expected a record per turn, got {}",
        turns.len()
    );

    // Turn 1: the model spoke instead of calling a tool. That is the failure mode that read as
    // "it did nothing" for four runs, because the text was never persisted anywhere.
    assert_eq!(turns[0].finish_reason, "prose");
    assert_eq!(
        turns[0].content.as_deref(),
        Some("Let me look at the config first."),
        "the model's own words must survive verbatim — this is the whole point"
    );
    assert!(turns[0].tool_calls.is_empty());

    // What it could reach, at the moment it chose. Answering this by hand meant reading
    // `catalog()` and `PathPolicy` and reasoning about which mode was active.
    assert!(
        turns[0].tools_offered.iter().any(|t| t == "write_file"),
        "offered tools must be recorded, got {:?}",
        turns[0].tools_offered
    );
    assert!(turns[0].message_count > 0);

    // Turn 2: it called a tool, and which one is on the record.
    assert_eq!(turns[1].finish_reason, "tool_calls");
    assert_eq!(turns[1].tool_calls, vec![SUBMIT_REPORT_TOOL.to_string()]);
}

/// Tool withdrawal is a guard decision that changes what the model can do; the record has to
/// show the catalog shrinking, or a run that "inexplicably stopped exploring" stays inexplicable.
#[tokio::test]
async fn the_record_shows_the_catalog_shrinking_when_a_guard_withdraws_a_tool() {
    let rec = Arc::new(Recorder::default());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        vec![
            call_tool("search"),
            call_tool("search"),
            call_tool("search"),
            call_tool("search"),
            submit(valid_report_args()),
        ],
    ));
    let exec = Executor::new(provider, Budget::default()).with_observer(rec.clone());
    let runtime = InvocationRecordingRuntime::new(&["search"], Ok("same result".into()));

    let _ = exec.execute(&runtime, Task::new("worker", "do it")).await;

    let turns = rec.turns.lock().unwrap().clone();
    let first = turns.first().expect("at least one turn");
    let last = turns.last().expect("at least one turn");
    assert!(
        first.tools_offered.iter().any(|t| t == "search"),
        "search should be offered at the start"
    );
    assert!(
        !last.tools_offered.iter().any(|t| t == "search"),
        "after the doom-loop guard removes it, the record must show it gone: {:?}",
        last.tools_offered
    );
}

#[test]
fn spill_oversized_result_under_threshold_passes_through() {
    let dir = tempfile::tempdir().unwrap();
    let text = "small result";
    let (shown, path) = spill_oversized_result(text, 1024, dir.path(), "test");
    assert_eq!(shown, text);
    assert!(path.is_none());
}

#[test]
fn spill_oversized_result_writes_file_and_keeps_the_tail() {
    let dir = tempfile::tempdir().unwrap();
    let spill = dir.path().join(".liberado").join("offload");
    let text = format!("{}MID{}", "A".repeat(3000), "Z".repeat(2000));
    let (shown, path) = spill_oversized_result(&text, 100, &spill, "call-1");
    assert!(shown.len() < text.len(), "preview shorter than body");
    assert!(shown.contains("truncated"));
    assert!(shown.contains("AAAA"), "head present");
    assert!(shown.contains("ZZZZ"), "tail present");
    let rel = path.expect("must return a path");
    assert_eq!(rel, ".liberado/offload/tool-spill-call-1.txt");
    let spilled = std::fs::read_to_string(spill.join("tool-spill-call-1.txt")).unwrap();
    assert_eq!(spilled, text);
}

#[test]
fn spill_oversized_result_distinct_labels_do_not_collide() {
    let dir = tempfile::tempdir().unwrap();
    let text = "oversized content".repeat(1000);
    let (_, a) = spill_oversized_result(&text, 10, dir.path(), "call-a");
    let (_, b) = spill_oversized_result(&text, 10, dir.path(), "call-b");
    assert_ne!(a, b);
    assert!(dir.path().join("tool-spill-call-a.txt").exists());
    assert!(dir.path().join("tool-spill-call-b.txt").exists());
}

#[test]
fn run_tool_spill_without_dir_passes_through_even_when_large() {
    let result = "big content!".repeat(10_000);
    let shown = run_tool_spill(&result, None, 100, "test");
    assert_eq!(shown, result, "no spill_dir must not truncate");
}

#[test]
fn run_tool_spill_writes_file_for_oversized() {
    let dir = tempfile::tempdir().unwrap();
    let result = "big content!".repeat(10_000);
    let shown = run_tool_spill(&result, Some(dir.path()), 100, "tool-call-1");
    assert!(shown.len() < result.len());
    assert!(shown.contains("tool-call-1"));
    let spilled = std::fs::read_to_string(dir.path().join("tool-spill-tool-call-1.txt")).unwrap();
    assert_eq!(spilled, result);
}
