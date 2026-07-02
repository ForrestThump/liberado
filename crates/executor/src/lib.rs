//! # liberado-executor
//!
//! The agent **execution engine**: a bounded, adaptive tool loop. Given a goal and a
//! [`ToolRuntime`] (the tools it may call and how to run them), it drives a [`Provider`] turn by
//! turn — the model proposes calls, we run them, feed the results back, and let it decide the next
//! step — until the task terminates.
//!
//! Termination follows the **consumer** of the output (the principle that justifies two modes
//! sharing one engine):
//!
//! * [`Executor::execute`] — *delegated* work whose consumer is another agent. The loop offers a
//!   synthetic [`SUBMIT_REPORT_TOOL`] whose argument schema *is* the [`Report`] schema; the model
//!   calling it both **terminates** the loop and **hands back** the typed [`Report`]. This is the
//!   path `ExecuteDirect`/`DispatchSubagent` take.
//! * [`Executor::converse`] — a *conversational* turn whose consumer is a human. Termination is
//!   implicit: the loop ends when the model replies with prose and no tool call, and that prose is
//!   the answer. No `Report`, because a person reads it.
//!
//! Two backstops keep the loop honest: a hard **turn budget** ([`Budget`]), and — in report mode —
//! a single nudge if the model answers without filing, after which its prose is wrapped as a
//! `Report` rather than lost. The actual MCP wiring (a turbomcp-backed [`ToolRuntime`]) and
//! threading write-provenance through it are deliberately *out* of this crate — the engine only
//! depends on the trait, so it is testable with a mock runtime and a [`MockProvider`].

mod risk_gated;

pub use risk_gated::RiskGatedToolRuntime;

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use liberado_common::{Outcome, Report, ToolCall};
use liberado_provider::{
    CompletionRequest, CompletionResponse, Message, Provider, ProviderError, Role, StreamItem,
    ToolDef, ToolInvocation,
};
use thiserror::Error;
use tokio::sync::mpsc::Sender;
use tracing::Instrument;

/// A high-level event emitted while [`Executor::converse_stream`] runs, for a client to render as it
/// happens. The executor itself emits [`Token`](AgentEvent::Token) and
/// [`ToolStarted`](AgentEvent::ToolStarted); the terminal [`Done`](AgentEvent::Done) /
/// [`Error`](AgentEvent::Error) are conventionally sent by the caller once the call returns.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// An incremental text delta of the answer.
    Token(String),
    /// A tool call is starting — its name and a compact preview of the arguments — emitted before
    /// the tool runs so the call is legible while it's in flight.
    ToolStarted { name: String, args: String },
    /// A tool call finished — its name, whether it succeeded, and a short preview of the result (or
    /// the error) — so the outcome is legible, not just the attempt.
    ToolFinished {
        name: String,
        ok: bool,
        preview: String,
    },
    /// The answer is complete.
    Done,
    /// Something failed.
    Error(String),
}

/// Cap a free-text preview (tool args or result) so a single chunky payload can't flood the stream
/// or the UI. Truncation is on `char` boundaries, with an ellipsis to signal there's more.
fn preview(text: &str) -> String {
    const MAX: usize = 200;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        let cut: String = text.chars().take(MAX).collect();
        format!("{cut}…")
    }
}

/// Name of the synthetic finish-tool the engine injects in report mode. A real [`ToolRuntime`]
/// must not expose a tool with this name (it would be shadowed by the engine's terminator).
pub const SUBMIT_REPORT_TOOL: &str = "submit_report";

/// Default turn budget. Generous enough for a multi-step subagent, bounded enough that a confused
/// model can't loop forever. `ExecuteDirect` should pass a tighter budget derived from
/// `small_fanout`.
pub const DEFAULT_MAX_TURNS: u32 = 8;

/// Appended once if the model answers in prose without filing a `Report`, asking it to do so.
const REPORT_NUDGE: &str = "Before finishing, call the `submit_report` tool with your final \
result. Do not reply in plain text.";

/// The tools available for a run plus how to execute them. Implemented by the (future)
/// turbomcp-backed runtime in production and by a mock in tests; the engine depends only on this.
#[async_trait]
pub trait ToolRuntime: Send + Sync {
    /// The tool catalog offered to the model this run. Capability narrowing happens *here* (a
    /// subagent's runtime only lists tools it is permitted to call), which is why the dispatcher's
    /// pre-flight guard over the classifier's opening move is a check, not the boundary.
    fn catalog(&self) -> Vec<ToolDef>;

    /// Execute one model-requested call and return the textual result fed back to the model.
    ///
    /// A **tool-level** failure is returned as `Err(message)`: the engine surfaces it to the model
    /// in-band (as the tool result) so it can adapt, exactly as a real agent would. Reserve hard
    /// errors (which abort the whole loop) for infrastructure faults, by surfacing them through the
    /// runtime's own state rather than here.
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String>;
}

/// Loop bounds. Currently just a turn cap; room to grow (token/time budgets) without touching call
/// sites.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Maximum model turns before the loop is force-terminated.
    pub max_turns: u32,
}

impl Budget {
    pub fn new(max_turns: u32) -> Self {
        Self { max_turns }
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_turns: DEFAULT_MAX_TURNS,
        }
    }
}

/// A unit of work for the engine: how to behave (`instructions`), what to do (`goal`), and an
/// optional classifier-provided opening move (`seed_calls`).
#[derive(Debug, Clone)]
pub struct Task {
    /// System prompt — role, constraints, and (in report mode) the instruction to finish via
    /// `submit_report`.
    pub instructions: String,
    /// The user/goal message.
    pub goal: String,
    /// The classifier's optional opening move (`ExecuteDirect::seed_calls`). Executed verbatim
    /// before the model's first turn, then the loop continues adaptively. Usually empty.
    pub seed_calls: Vec<ToolCall>,
}

impl Task {
    pub fn new(instructions: impl Into<String>, goal: impl Into<String>) -> Self {
        Self {
            instructions: instructions.into(),
            goal: goal.into(),
            seed_calls: Vec::new(),
        }
    }

    /// Seed the loop with an opening move (the classifier's pre-planned first calls).
    pub fn with_seed(mut self, seed_calls: Vec<ToolCall>) -> Self {
        self.seed_calls = seed_calls;
        self
    }
}

/// Errors that abort a run. Tool-level failures are *not* here — they are fed back to the model
/// in-band (see [`ToolRuntime::invoke`]). A budget hit is surfaced as an `Err` from the core loop
/// but [`Executor::execute`] maps it to a `Failed` [`Report`], since the delegating agent is owed a
/// Report rather than a transport error.
#[derive(Debug, Error)]
pub enum ExecError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("the model's submit_report arguments did not match the Report schema: {0}")]
    Decode(String),
    #[error("execution exceeded the {turns}-turn budget")]
    BudgetExceeded { turns: u32 },
    #[error("internal executor invariant violated: {0}")]
    Internal(&'static str),
}

/// How a run terminates internally; each public mode yields exactly one variant.
enum Terminal {
    Filed(Report),
    Spoke(String),
}

#[derive(Clone, Copy)]
enum Mode {
    Report,
    Conversational,
}

/// The bounded, adaptive tool-loop engine. Cheap to clone-share via the inner `Arc`.
#[derive(Clone)]
pub struct Executor {
    provider: Arc<dyn Provider>,
    budget: Budget,
}

impl Executor {
    pub fn new(provider: Arc<dyn Provider>, budget: Budget) -> Self {
        Self { provider, budget }
    }

    /// Run delegated work to a typed [`Report`] (report mode). The model finishes by calling
    /// [`SUBMIT_REPORT_TOOL`]. A budget exhaustion becomes a `Failed` Report (the caller is owed
    /// one); provider/decode faults propagate as [`ExecError`].
    pub async fn execute(
        &self,
        runtime: &dyn ToolRuntime,
        task: Task,
    ) -> Result<Report, ExecError> {
        let span = tracing::info_span!(
            "execute",
            mode = "report",
            goal = %task.goal,
            budget = self.budget.max_turns,
            has_seed = !task.seed_calls.is_empty(),
            outcome = tracing::field::Empty,
        );
        async {
            match self.drive(runtime, task, Mode::Report).await {
                Ok(Terminal::Filed(report)) => {
                    tracing::Span::current()
                        .record("outcome", &format_args!("{:?}", report.outcome));
                    tracing::info!(summary = %report.summary, "execution filed report");
                    Ok(report)
                }
                Ok(Terminal::Spoke(_)) => {
                    tracing::Span::current().record("outcome", "internal_error");
                    Err(ExecError::Internal("report mode returned prose"))
                }
                Err(ExecError::BudgetExceeded { turns }) => {
                    tracing::Span::current().record("outcome", "budget_exceeded");
                    tracing::warn!(turns, "execution budget exceeded; returning failed report");
                    Ok(budget_failed_report(turns))
                }
                Err(e) => {
                    tracing::Span::current().record("outcome", "error");
                    tracing::error!(error = %e, "execution aborted");
                    Err(e)
                }
            }
        }
        .instrument(span)
        .await
    }

    /// Run a conversational turn to a prose answer (conversational mode). Terminates when the model
    /// replies without a tool call. A budget hit propagates as [`ExecError::BudgetExceeded`].
    pub async fn converse(
        &self,
        runtime: &dyn ToolRuntime,
        task: Task,
    ) -> Result<String, ExecError> {
        let span = tracing::info_span!(
            "converse",
            mode = "conversational",
            goal = %task.goal,
            budget = self.budget.max_turns,
            outcome = tracing::field::Empty,
        );
        async {
            match self.drive(runtime, task, Mode::Conversational).await {
                Ok(Terminal::Spoke(text)) => {
                    tracing::Span::current().record("outcome", "spoke");
                    tracing::info!("conversation completed");
                    Ok(text)
                }
                Ok(Terminal::Filed(_)) => {
                    tracing::Span::current().record("outcome", "internal_error");
                    Err(ExecError::Internal("conversational mode filed a report"))
                }
                Err(e) => {
                    tracing::Span::current().record("outcome", "error");
                    tracing::error!(error = %e, "conversation aborted");
                    Err(e)
                }
            }
        }
        .instrument(span)
        .await
    }

    /// Build the initial conversation from `task`, then run the loop.
    async fn drive(
        &self,
        runtime: &dyn ToolRuntime,
        task: Task,
        mode: Mode,
    ) -> Result<Terminal, ExecError> {
        let mut messages = vec![Message::system(task.instructions), Message::user(task.goal)];

        let mut tools = runtime.catalog();
        if matches!(mode, Mode::Report) {
            tools.push(submit_report_tool());
        }

        // The classifier's opening move, executed as if the model had emitted it.
        self.run_seed(runtime, &mut messages, &task.seed_calls)
            .await;

        self.run_loop(runtime, &mut messages, &tools, mode).await
    }

    /// Run a conversational turn over an existing message history (multi-turn chat). The caller owns
    /// `messages` — the system prompt, prior turns, and the new user message — and this drives the
    /// model + tools until it replies in prose, appending every turn (including tool calls/results)
    /// so context carries forward, and returns that prose. No `submit_report` (the consumer is a
    /// human, so prose *is* the answer — termination follows the consumer, like [`converse`]).
    ///
    /// [`converse`]: Self::converse
    pub async fn converse_messages(
        &self,
        runtime: &dyn ToolRuntime,
        messages: &mut Vec<Message>,
    ) -> Result<String, ExecError> {
        let tools = runtime.catalog();
        match self
            .run_loop(runtime, messages, &tools, Mode::Conversational)
            .await?
        {
            Terminal::Spoke(text) => Ok(text),
            Terminal::Filed(_) => Err(ExecError::Internal("conversational mode filed a report")),
        }
    }

    /// Streaming multi-turn chat: like [`converse_messages`](Self::converse_messages), but emits
    /// [`AgentEvent`]s as they happen — answer tokens as the model produces them, and a
    /// `ToolStarted` before each tool call — over `events`. The history in `messages` is updated as
    /// it goes (model turns + tool results), so the conversation carries forward. Returns when the
    /// model replies in prose (the answer was streamed) or the budget is exhausted. The caller sends
    /// the terminal `Done`/`Error` based on the result.
    pub async fn converse_stream(
        &self,
        runtime: &dyn ToolRuntime,
        messages: &mut Vec<Message>,
        events: &Sender<AgentEvent>,
    ) -> Result<(), ExecError> {
        let tools = runtime.catalog();
        for turn in 1..=self.budget.max_turns {
            let request = CompletionRequest::new(messages.clone()).with_tools(tools.clone());
            let mut stream = self.provider.complete_stream(request).await?;

            let mut response = None;
            while let Some(item) = stream.next().await {
                match item? {
                    StreamItem::Token(text) => {
                        // A dropped receiver (client disconnected) just means no one is listening.
                        let _ = events.send(AgentEvent::Token(text)).await;
                    }
                    StreamItem::Done(resp) => response = Some(resp),
                }
            }
            let response =
                response.ok_or(ExecError::Internal("stream ended without a final response"))?;

            messages.push(assistant_turn(&response));
            if response.tool_calls.is_empty() {
                return Ok(()); // the prose answer was streamed as tokens
            }

            for call in &response.tool_calls {
                // A dropped receiver (client disconnected) just means no one is listening.
                let _ = events
                    .send(AgentEvent::ToolStarted {
                        name: call.name.clone(),
                        args: preview(&call.arguments.to_string()),
                    })
                    .await;
                // Invoke directly (not via `run_tool`) so the outcome's ok/err is legible as its own
                // event; the history still gets the same string `run_tool` would have produced.
                let (ok, result) = match runtime.invoke(call).await {
                    Ok(content) => (true, content),
                    Err(message) => (false, format!("tool error: {message}")),
                };
                let _ = events
                    .send(AgentEvent::ToolFinished {
                        name: call.name.clone(),
                        ok,
                        preview: preview(&result),
                    })
                    .await;
                messages.push(Message::tool_result(&call.id, result));
            }
            tracing::info!(turn, tools = response.tool_calls.len(), "turn used tools");
        }

        Err(ExecError::BudgetExceeded {
            turns: self.budget.max_turns,
        })
    }

    /// The turn loop shared by [`drive`](Self::drive) and
    /// [`converse_messages`](Self::converse_messages): provider call → record the turn → on prose,
    /// terminate per `mode`; on tool calls, run them and continue — until the turn budget.
    async fn run_loop(
        &self,
        runtime: &dyn ToolRuntime,
        messages: &mut Vec<Message>,
        tools: &[ToolDef],
        mode: Mode,
    ) -> Result<Terminal, ExecError> {
        let mut nudged = false;
        for turn in 1..=self.budget.max_turns {
            let turn_span = tracing::debug_span!(
                "turn",
                turn,
                tool_calls = tracing::field::Empty,
                finish_reason = tracing::field::Empty,
            );
            let response = async {
                let request = CompletionRequest::new(messages.clone()).with_tools(tools.to_vec());
                self.provider.complete(request).await
            }
            .instrument(tracing::debug_span!("provider_complete", turn))
            .await?;

            // Record the model's turn (content and/or tool calls) so it sees its own history.
            messages.push(assistant_turn(&response));

            if response.tool_calls.is_empty() {
                let text = response.content.unwrap_or_default();
                turn_span.record("finish_reason", "prose");
                match mode {
                    Mode::Conversational => return Ok(Terminal::Spoke(text)),
                    Mode::Report if !nudged => {
                        nudged = true;
                        tracing::debug!(
                            turn,
                            "model replied with prose; nudging to use submit_report"
                        );
                        messages.push(Message::user(REPORT_NUDGE));
                        continue;
                    }
                    Mode::Report => {
                        tracing::warn!(
                            turn,
                            "executor finished without submit_report; wrapping prose as Report"
                        );
                        return Ok(Terminal::Filed(prose_report(text)));
                    }
                }
            }

            let tool_count = response.tool_calls.len();
            turn_span.record("tool_calls", tool_count);
            turn_span.record("finish_reason", "tool_calls");
            if tool_count > 0 {
                let names: Vec<&str> = response
                    .tool_calls
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect();
                tracing::info!(turn, tool_count, ?names, "turn called tools");
            }

            for call in &response.tool_calls {
                if call.name == SUBMIT_REPORT_TOOL {
                    tracing::info!(turn, "subagent filed report");
                    let report = serde_json::from_value::<Report>(call.arguments.clone())
                        .map_err(|e| ExecError::Decode(e.to_string()))?;
                    return Ok(Terminal::Filed(report));
                }
                let tool_span = tracing::debug_span!("tool_call", name = %call.name, id = %call.id);
                let result = async { run_tool(runtime, call).await }
                    .instrument(tool_span)
                    .await;
                messages.push(Message::tool_result(&call.id, result));
            }
        }

        tracing::warn!(turns = self.budget.max_turns, "execution budget exhausted");
        Err(ExecError::BudgetExceeded {
            turns: self.budget.max_turns,
        })
    }

    /// Execute the classifier's seed calls and append the synthetic assistant turn + results, so
    /// the model continues from a coherent transcript. No-op when there are no seed calls.
    async fn run_seed(
        &self,
        runtime: &dyn ToolRuntime,
        messages: &mut Vec<Message>,
        seed_calls: &[ToolCall],
    ) {
        if seed_calls.is_empty() {
            return;
        }
        let count = seed_calls.len();
        tracing::debug!(count, "executing seed calls");
        let invocations: Vec<ToolInvocation> = seed_calls
            .iter()
            .enumerate()
            .map(|(i, c)| ToolInvocation::new(format!("seed-{i}"), &c.tool, c.args.clone()))
            .collect();

        messages.push(Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: invocations.clone(),
            tool_call_id: None,
        });
        for inv in &invocations {
            let span = tracing::debug_span!("seed_call", tool = %inv.name, id = %inv.id);
            let result = async { run_tool(runtime, inv).await }
                .instrument(span)
                .await;
            messages.push(Message::tool_result(&inv.id, result));
        }
    }
}

/// Run one tool call, folding a tool-level error into an in-band result string so the model can
/// adapt rather than the loop aborting.
async fn run_tool(runtime: &dyn ToolRuntime, call: &ToolInvocation) -> String {
    match runtime.invoke(call).await {
        Ok(content) => content,
        Err(message) => format!("tool error: {message}"),
    }
}

/// Reconstruct the assistant message from a completion response (content + requested tool calls).
fn assistant_turn(response: &CompletionResponse) -> Message {
    Message {
        role: Role::Assistant,
        content: response.content.clone().unwrap_or_default(),
        tool_calls: response.tool_calls.clone(),
        tool_call_id: None,
    }
}

/// The synthetic finish-tool: its parameter schema mirrors [`Report`], so the model's call args
/// deserialize straight into one.
fn submit_report_tool() -> ToolDef {
    ToolDef::new(
        SUBMIT_REPORT_TOOL,
        "Finish the task and hand back a structured report. Call this exactly once, when done.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "outcome": {
                    "type": "string",
                    "enum": ["succeeded", "partially_succeeded", "failed", "proposed"],
                    "description": "Terminal status of the work."
                },
                "summary": {
                    "type": "string",
                    "description": "High-signal, human-readable, short."
                },
                "artifacts": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Vault paths written, e.g. \"reviews/2026-06-21.md\"."
                },
                "new_high_signal_facts": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Things worth surfacing into context."
                },
                "follow_up": {
                    "type": "string",
                    "description": "Optional suggested next step for the main agent."
                }
            },
            "required": ["outcome", "summary"]
        }),
    )
}

/// The `Report` synthesized when the model answers in prose and won't file even after a nudge. The
/// prose is preserved as the summary; outcome is optimistic because a plain stop after work
/// generally means "done, here's the answer."
fn prose_report(summary: String) -> Report {
    Report {
        outcome: Outcome::Succeeded,
        summary,
        artifacts: Vec::new(),
        new_high_signal_facts: Vec::new(),
        follow_up: None,
    }
}

/// The `Report` returned when the turn budget is exhausted without completion.
fn budget_failed_report(turns: u32) -> Report {
    Report {
        outcome: Outcome::Failed,
        summary: format!("Execution exceeded the {turns}-turn budget without completing."),
        artifacts: Vec::new(),
        new_high_signal_facts: Vec::new(),
        follow_up: Some("Consider dispatching a subagent with a larger budget.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_provider::{CompletionResponse, MockProvider};
    use std::sync::Mutex;

    /// A `ToolRuntime` that offers a fixed catalog, records every invocation, and returns a canned
    /// result for any call.
    struct MockToolRuntime {
        tools: Vec<ToolDef>,
        invoked: Mutex<Vec<ToolInvocation>>,
        result: Result<String, String>,
    }

    impl MockToolRuntime {
        fn new(tool_names: &[&str], result: Result<String, String>) -> Self {
            let tools = tool_names
                .iter()
                .map(|n| ToolDef::new(*n, "test tool", serde_json::json!({ "type": "object" })))
                .collect();
            Self {
                tools,
                invoked: Mutex::new(Vec::new()),
                result,
            }
        }

        fn invoked(&self) -> Vec<ToolInvocation> {
            self.invoked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ToolRuntime for MockToolRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            self.tools.clone()
        }
        async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
            self.invoked.lock().unwrap().push(call.clone());
            self.result.clone()
        }
    }

    fn call_tool(name: &str) -> CompletionResponse {
        CompletionResponse::tool_calls(vec![ToolInvocation::new("c", name, serde_json::json!({}))])
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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));
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
        let runtime = MockToolRuntime::new(&["search"], Err("boom".into()));
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
    async fn budget_exhaustion_becomes_a_failed_report() {
        // Two tool turns, never files; budget of 2 forces termination.
        let (_provider, exec) = executor(
            vec![call_tool("search"), call_tool("search")],
            Budget::new(2),
        );
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

        let report = exec
            .execute(&runtime, Task::new("worker", "loop forever"))
            .await
            .unwrap();

        assert_eq!(report.outcome, Outcome::Failed);
        assert!(report.summary.contains("budget"));
    }

    #[tokio::test]
    async fn malformed_submit_report_args_is_a_decode_error() {
        // Missing the required `summary` field.
        let (_provider, exec) = executor(
            vec![submit(serde_json::json!({ "outcome": "succeeded" }))],
            Budget::default(),
        );
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

        let err = exec
            .execute(&runtime, Task::new("worker", "do it"))
            .await
            .unwrap_err();

        assert!(matches!(err, ExecError::Decode(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn seed_calls_run_before_the_first_model_turn() {
        let (provider, exec) = executor(vec![submit(valid_report_args())], Budget::default());
        let runtime = MockToolRuntime::new(&["tasks-mcp:add"], Ok("added".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Err("upstream 500".into()));

        let report = exec
            .execute(&runtime, Task::new("worker", "do it"))
            .await
            .unwrap();

        // The run completed (no abort) and the tool was still invoked.
        assert_eq!(report.outcome, Outcome::Succeeded);
        assert_eq!(runtime.invoked().len(), 1);
    }
}
