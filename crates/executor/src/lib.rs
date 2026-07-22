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
use liberado_common::{Outcome, Report, ToolCall, WriteProvenance};
use liberado_provider::{
    CompletionRequest, CompletionResponse, Message, Provider, ProviderError, Role, StreamItem,
    ToolDef, ToolInvocation,
};
use liberado_scratchpad::{SCRATCHPAD_TOOL, Scratchpad};
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

/// Appended once if the model answers in prose without filing a `Report`. Deliberately offers
/// *both* options (keep going, or finish) rather than unconditionally pushing to wrap up — an
/// earlier wording ("Before finishing, call `submit_report`...") biased a model that paused to
/// narrate mid-plan toward prematurely filing instead of continuing a genuinely multi-step goal, a
/// real live finding from `liberado-heuristics-tuner`'s executor-layer tuning (a scenario needing
/// two distinct tool calls scored 0/6 across two independent runs, even under system prompts that
/// explicitly instructed both calls — the nudge's own wording was working against the prompt at
/// exactly the moment it mattered, docs/roadmap/heuristics-tuning-engine-plan.md).
const REPORT_NUDGE: &str = "If the goal isn't finished yet, continue by calling whatever tool you \
still need — don't stop partway through a multi-step plan. Once it's actually done (or you \
genuinely cannot proceed), call `submit_report` with your final result. Do not reply in plain text.";

/// How many consecutive, *near-duplicate* invocations of the same tool count as a "doom loop" — the
/// model succeeding at a tool call every time yet making no progress, rather than hitting an error
/// it could react to. Matches the threshold comparable harnesses use for the same failure mode
/// (opencode/kilocode's `DOOM_LOOP_THRESHOLD`, VTCode's `LoopDetector`) — evidence this needs an
/// engine-level guard, not just prompt wording, came from a live reproduction of
/// `docs/roadmap/multi-step-execution-reliability-finding.md`: DeepSeek and Gemini both got stuck
/// calling `deepwiki` 3-6 times in a row (every call succeeded; the result was just an unhelpful,
/// repeatable answer) and never reached the second required tool, burning the whole turn budget. A
/// tool call *succeeding* every time denies the model the one signal ("that failed") it reliably
/// adapts to; whether it *also* notices "repeating this won't help" is a subtler, less reliable
/// judgment call — and even a model that would eventually notice doesn't get the chance inside
/// Liberado's tight turn budgets (4 for `ExecuteDirect`).
///
/// "Near-duplicate" matters, not just "identical": a first cut of this guard checked byte-for-byte
/// argument equality and it did not fire against the real failure above, because the model was
/// rephrasing the same question each call (`"turbomcp transport layer"` ->
/// `"turbo-mcp transport Provider trait stdio HTTP JSON-RPC MCP protocol"` -> ...) rather than
/// repeating it verbatim. See `args_similarity`.
const DOOM_LOOP_THRESHOLD: usize = 3;

/// Minimum cosine similarity (see `args_similarity`) between consecutive same-tool calls' arguments
/// for them to count as "the same call" for [`DOOM_LOOP_THRESHOLD`] purposes. Hand-calibrated
/// against the two cases that matter, not a large corpus (still just a starting point, revisit if
/// live use shows false positives/negatives): the real DeepSeek transcript's 3 rephrasings of the
/// same question scored ~0.26/~0.41/~0.24 pairwise, while 3 genuinely distinct queries to the same
/// tool ("weather in Denver" / "capital of France" / "current bitcoin price") scored ~0.10. `0.2`
/// sits between those clusters — closer to the distinct-queries side, since a missed detection just
/// costs one more turn before the next check, while a false positive would nudge the model away from
/// legitimately varied, on-track work.
const ARG_SIMILARITY_THRESHOLD: f32 = 0.2;

/// A small, one-time top-up to the turn budget, granted only when the tool-removal escalation step
/// (strike 2, see the guard block in `run_loop`) actually fires — not a general "loops are free"
/// refund. opencode/kilocode/VTCode were all checked directly and none of them refund or extend the
/// budget just because a loop was detected; recovery counts against the original cap in all three.
/// This is narrower and for a different reason: live evidence showed tool removal is structurally
/// useless without it. `ExecuteDirect`'s 4-turn budget lets the nudge fire at turn 3 and removal at
/// turn 4 — the *last* turn — leaving zero turns for the model to actually use what removal freed it
/// to do, so the mechanism could never once pay off. Bounded and granted at most once per run (see
/// `bonus_granted` in `run_loop`), so total worst-case turns stay capped at
/// `max_turns + DOOM_LOOP_RECOVERY_BONUS_TURNS`, never unbounded.
const DOOM_LOOP_RECOVERY_BONUS_TURNS: u32 = 2;

/// The 3-step escalation ladder (1st -> nudge, 2nd -> remove, 3rd+ -> give up) for one loop-detection
/// mechanism. `run_loop` keeps one `LoopGuard` per mechanism (doom-loop, short-cycle) rather than a
/// single counter shared between them — an earlier version shared one `loop_strikes: u8` across
/// both, so whichever mechanism detected a problem *second* silently skipped its own nudge step
/// whenever the other had already struck once (e.g. a short cycle nudging first meant the very next,
/// entirely unrelated doom-loop detection jumped straight to tool removal, never having nudged for
/// that behavior at all). The one-time turn-budget top-up (`DOOM_LOOP_RECOVERY_BONUS_TURNS`) stays a
/// single `bonus_granted` flag shared by both in `run_loop`, since that grant is genuinely per-run,
/// not per-mechanism.
#[derive(Default)]
struct LoopGuard {
    strikes: u8,
}

/// What a [`LoopGuard`] says to do in response to its mechanism detecting a problem again.
enum Escalation {
    Nudge,
    Remove,
    GiveUp,
}

impl LoopGuard {
    fn strike(&mut self) -> Escalation {
        self.strikes += 1;
        match self.strikes {
            1 => Escalation::Nudge,
            2 => Escalation::Remove,
            _ => Escalation::GiveUp,
        }
    }
}

/// The first, softest escalation step when the doom-loop guard fires — mirrors `REPORT_NUDGE`'s
/// nudge shape: engine-level, independent of whatever `DIRECT_INSTRUCTIONS`/`SUBAGENT_PREAMBLE`
/// text the tuner eventually settles on. If it fires again, the guard stops asking and starts
/// removing the offending tool instead (see the guard block in `run_loop`) — live testing showed
/// this alone doesn't change DeepSeek/Gemini's behavior (they repeated a 4th time anyway, with zero
/// visible acknowledgment of the nudge in their response content), so it's a first try, not the
/// whole mechanism.
const DOOM_LOOP_NUDGE: &str = "You've called the same tool with the same or very similar arguments \
several times in a row without new information. Use the result you already have to take the next \
step in the plan, or call `submit_report` if you're genuinely stuck — repeating that call again \
will not help.";

/// The first escalation step for the second failure shape this guard catches: alternating between
/// the same short cycle of tools (A, B, A, B, ...) instead of a repeated single call. VTCode's
/// `LoopDetector` calls this pattern out explicitly (`detect_patterns`) as distinct from a single
/// tool repeating — worth guarding even without live evidence of it happening yet, since the
/// detection is essentially free (exact tool-name matching over the same call history this guard
/// already tracks) and the underlying risk (burning the turn budget without progress) is identical.
const CYCLE_NUDGE: &str = "You're alternating between the same short cycle of tools without making \
new progress. Break the cycle: use what you already have to take a genuinely different next step, \
or call `submit_report` if you're stuck.";

/// The second escalation step for a persisting doom loop: the offending tool is actually removed
/// from what the model can call for the rest of this task, not just asked to stop. Telling the
/// model this explicitly (rather than silently shrinking its catalog) keeps the transcript
/// coherent — a tool disappearing with no explanation would otherwise look like an error.
fn tool_removed_nudge(tool_name: &str) -> String {
    format!(
        "The `{tool_name}` tool has been removed for the rest of this task — repeating it wasn't \
         producing new information. Use the result(s) you already have to make progress with your \
         remaining tools, or call `submit_report` if nothing else can move the goal forward."
    )
}

/// The second escalation step for a persisting tool-cycle — see [`tool_removed_nudge`]'s doc
/// comment for why the model is told, not just silently restricted.
fn tools_removed_nudge(tool_names: &[String]) -> String {
    let list = tool_names.join("`, `");
    format!(
        "The `{list}` tool(s) have been removed for the rest of this task — cycling between them \
         wasn't making progress. Use what you already have with your remaining tools, or call \
         `submit_report` if nothing else can move the goal forward."
    )
}

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

/// Failure building a [`ToolRuntime`] for an execution (connection/handshake/etc.).
#[derive(Debug, Error)]
#[error("{0}")]
pub struct RuntimeSetupError(pub String);

/// How an orchestrator obtains a [`ToolRuntime`] for an execution: given the MCPs the execution is
/// allowed to see and the provenance every call should carry, return a connected runtime. The real
/// implementation (turbomcp-backed) lives in the MCP layer; tests inject a mock. Lives here (rather
/// than in `liberado-orchestrator`, which consumes it) so `liberado-mcp` — which implements it — only
/// needs to depend on this crate, not sideways into the dispatch-bridging one.
#[async_trait]
pub trait RuntimeFactory: Send + Sync {
    async fn runtime_for(
        &self,
        allowed_mcps: &[String],
        provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError>;
}

/// A single bounded resource an execution must respect, checked once per turn against the
/// accumulated [`ResourceUsage`] snapshot. New resource types (a rate limit, anything else
/// bounded) just implement this — `Executor::run_loop`'s own logic never has to change to add
/// one, only a new [`Budget::with_limit`] call site does. Deliberately abstract rather than a
/// hardcoded enum of resource kinds: today's two concrete uses (wall-clock, a token-count proxy
/// for cost — see [`TokenLimit`]'s doc comment for why not real dollars yet) shouldn't be the
/// ceiling on what this can bound later.
pub trait ResourceLimit: Send + Sync {
    /// Human-readable name for diagnostics ("wall-clock", "tokens") — surfaced in a budget-
    /// exceeded failure report so it names *which* resource ran out, not just "turns."
    fn name(&self) -> &str;
    /// Whether this resource has been exhausted given the current usage snapshot.
    fn is_exhausted(&self, usage: &ResourceUsage) -> bool;
}

/// Accumulated resource usage for one execution, updated once per turn. Adding a new
/// [`ResourceLimit`] later may need a new field here — a small, additive change; existing limits
/// and `run_loop`'s own logic don't need to change alongside it.
#[derive(Debug, Clone, Default)]
pub struct ResourceUsage {
    pub turns: u32,
    pub elapsed: std::time::Duration,
    /// Total tokens (prompt + completion) spent so far — see [`TokenLimit`]'s doc comment.
    pub tokens: u64,
}

/// Bounds real elapsed time, independent of turn count — a single slow tool call or a model that
/// just takes a long time per turn isn't caught by a turn cap alone.
pub struct WallClockLimit(pub std::time::Duration);

impl ResourceLimit for WallClockLimit {
    fn name(&self) -> &str {
        "wall-clock"
    }
    fn is_exhausted(&self, usage: &ResourceUsage) -> bool {
        usage.elapsed >= self.0
    }
}

/// A stand-in for a real dollar-cost cap: total token count, not actual `$`. Real pricing needs a
/// per-model `$`/token table (rates differ by provider and by prompt vs. completion token, and
/// need upkeep as providers change prices) that doesn't exist yet — deferred until it's clearly
/// worth that upkeep, since current model usage is cheap enough not to need it now. Token count is
/// a reasonable proxy in the meantime: it already correlates with real cost, and it's free (every
/// `CompletionResponse` already reports it) — no new plumbing to add real dollars later either,
/// just a new `ResourceLimit` impl reading a pricing table instead of a raw count.
pub struct TokenLimit(pub u64);

impl ResourceLimit for TokenLimit {
    fn name(&self) -> &str {
        "tokens"
    }
    fn is_exhausted(&self, usage: &ResourceUsage) -> bool {
        usage.tokens >= self.0
    }
}

/// Loop bounds: a turn cap (`max_turns`, unchanged from before — still the mechanical driver of
/// `run_loop`'s own iteration, including the doom-loop guard's one-time recovery top-up, which is
/// specifically a turn-count adjustment) plus an open-ended list of additional [`ResourceLimit`]s
/// checked alongside it every turn. `Budget::new`/`Budget::default` build a turns-only budget —
/// unchanged behavior for every existing call site — `.with_limit`/`.with_wall_clock`/
/// `.with_token_limit` opt a call site into additional bounds.
#[derive(Clone)]
pub struct Budget {
    /// Maximum model turns before the loop is force-terminated.
    pub max_turns: u32,
    extra_limits: Arc<Vec<Box<dyn ResourceLimit>>>,
}

impl Budget {
    pub fn new(max_turns: u32) -> Self {
        Self {
            max_turns,
            extra_limits: Arc::new(Vec::new()),
        }
    }

    /// Add an arbitrary [`ResourceLimit`] to this budget, checked every turn alongside the turn
    /// cap. Chainable: `Budget::new(4).with_limit(WallClockLimit(...)).with_limit(TokenLimit(...))`.
    pub fn with_limit(mut self, limit: impl ResourceLimit + 'static) -> Self {
        // `Arc::get_mut` (not `make_mut`, which needs `T: Clone` — trait objects don't support
        // that generically) succeeds whenever this is the only reference, true for every real
        // call site (builder chains are used immediately: `Budget::new(4).with_wall_clock(...)`).
        // The `None` arm is only reachable if a `Budget` were cloned mid-chain before finishing —
        // doesn't happen anywhere in this codebase, but falls back to starting a fresh list
        // rather than panicking if it ever did.
        match Arc::get_mut(&mut self.extra_limits) {
            Some(limits) => limits.push(Box::new(limit)),
            None => self.extra_limits = Arc::new(vec![Box::new(limit)]),
        }
        self
    }

    /// Shorthand for `with_limit(WallClockLimit(max))`.
    pub fn with_wall_clock(self, max: std::time::Duration) -> Self {
        self.with_limit(WallClockLimit(max))
    }

    /// Shorthand for `with_limit(TokenLimit(max_tokens))`.
    pub fn with_token_limit(self, max_tokens: u64) -> Self {
        self.with_limit(TokenLimit(max_tokens))
    }

    /// The name of the first exhausted extra limit (wall-clock, tokens, ...), if any — `None`
    /// means none of the *extra* limits are exhausted (the turn cap is checked separately, since
    /// it's the loop's own mechanical bound, not one of these).
    fn exhausted_extra(&self, usage: &ResourceUsage) -> Option<&str> {
        self.extra_limits
            .iter()
            .find(|limit| limit.is_exhausted(usage))
            .map(|limit| limit.name())
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_TURNS)
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
            model = %self.provider.model(),
            goal = %task.goal,
            budget = self.budget.max_turns,
            has_seed = !task.seed_calls.is_empty(),
            outcome = tracing::field::Empty,
        );
        async {
            match self.drive(runtime, task, Mode::Report).await {
                Ok(Terminal::Filed(report)) => {
                    tracing::Span::current()
                        .record("outcome", format_args!("{:?}", report.outcome));
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
            model = %self.provider.model(),
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
            tools.push(Scratchpad::tool_def());
        }
        let mut scratchpad = matches!(mode, Mode::Report).then(Scratchpad::new);

        // The classifier's opening move, executed as if the model had emitted it.
        self.run_seed(runtime, &mut messages, &task.seed_calls)
            .await;

        self.run_loop(runtime, &mut messages, &mut tools, mode, &mut scratchpad)
            .await
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
        let span = tracing::info_span!(
            "converse_messages",
            model = %self.provider.model(),
            budget = self.budget.max_turns,
        );
        async {
            let mut tools = runtime.catalog();
            // Conversational mode gets no scratchpad this pass (see liberado-scratchpad's module
            // docs) — the call site is ready for it, just not enabled yet.
            let mut scratchpad: Option<Scratchpad> = None;
            match self
                .run_loop(
                    runtime,
                    messages,
                    &mut tools,
                    Mode::Conversational,
                    &mut scratchpad,
                )
                .await?
            {
                Terminal::Spoke(text) => Ok(text),
                Terminal::Filed(_) => {
                    Err(ExecError::Internal("conversational mode filed a report"))
                }
            }
        }
        .instrument(span)
        .await
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
        let span = tracing::info_span!(
            "converse_stream",
            model = %self.provider.model(),
            budget = self.budget.max_turns,
        );
        let _enter = span.enter();
        tracing::debug!(model = %self.provider.model(), "starting conversational stream turn");
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
        tools: &mut Vec<ToolDef>,
        mode: Mode,
        scratchpad: &mut Option<Scratchpad>,
    ) -> Result<Terminal, ExecError> {
        let mut nudged = false;
        // (tool name, arguments, result) of every real invocation, in call order, across the whole
        // run — not just within one turn, since the doom loop this guards against spans turns (see
        // `DOOM_LOOP_THRESHOLD`'s doc comment). The result rides along too so a budget-exhaustion
        // failure report can show what actually happened instead of a bare "ran out of turns" —
        // see `budget_failed_report_with_progress`.
        let mut call_history: Vec<(String, serde_json::Value, String)> = Vec::new();
        // One escalation ladder per mechanism (see `LoopGuard`'s doc comment for why these must NOT
        // share a counter): 1st detection -> nudge, 2nd -> remove the offending tool(s) and explain
        // why, 3rd+ -> give up honestly. Removal (not just another nudge) is the second step because
        // a nudge alone did not change DeepSeek/Gemini's behavior in live testing — they repeated
        // anyway — so the next escalation needs to change what's *possible*, not just what's *said*.
        let mut doom_guard = LoopGuard::default();
        let mut cycle_guard = LoopGuard::default();
        // Mutable so the tool-removal escalation step can grant its one-time top-up (see
        // `DOOM_LOOP_RECOVERY_BONUS_TURNS`); `bonus_granted` caps that to once per run. Distinct
        // from `usage`/`extra_limits` below: the turn cap is the loop's own mechanical bound (it
        // drives the `for`-equivalent iteration itself), the extra limits are additional,
        // independently-checked bounds layered on top.
        let mut max_turns = self.budget.max_turns;
        let mut bonus_granted = false;
        let mut turn: u32 = 0;
        let mut usage = ResourceUsage::default();
        let run_started = std::time::Instant::now();
        let mut exhausted_resource: Option<&str> = None;
        'turn_loop: loop {
            turn += 1;
            usage.turns = turn;
            usage.elapsed = run_started.elapsed();
            if turn > max_turns {
                break;
            }
            if let Some(name) = self.budget.exhausted_extra(&usage) {
                exhausted_resource = Some(name);
                break;
            }
            let turn_span = tracing::debug_span!(
                "turn",
                turn,
                tool_calls = tracing::field::Empty,
                finish_reason = tracing::field::Empty,
            );
            let response = async {
                let request = CompletionRequest::new(messages.clone()).with_tools(tools.clone());
                self.provider.complete(request).await
            }
            .instrument(tracing::debug_span!("provider_complete", turn))
            .await?;

            if let Some(response_usage) = &response.usage {
                usage.tokens += u64::from(response_usage.total_tokens);
            }

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
                if let Some(content) = &response.content
                    && !content.is_empty()
                {
                    tracing::info!(turn, %content, "model's reasoning alongside the tool call(s)");
                }
            }

            // Process every tool call in this turn *before* doom/cycle escalations that jump to
            // the next provider call. OpenAI-compat providers require one tool-result message per
            // `tool_call_id` after an assistant tool_calls message (dogfood D3, 01KX7AGD).
            let mut submitted_report: Option<Report> = None;
            let mut doom_hit: Option<String> = None;
            let mut cycle_hit: Option<Vec<String>> = None;
            for call in &response.tool_calls {
                if call.name == SUBMIT_REPORT_TOOL {
                    tracing::info!(turn, "subagent filed report");
                    // Still emit a tool result so the transcript is well-formed if we ever
                    // re-use `messages` after this; then stop after the batch.
                    messages.push(Message::tool_result(
                        &call.id,
                        "report accepted".to_string(),
                    ));
                    match serde_json::from_value::<Report>(call.arguments.clone()) {
                        Ok(report) => submitted_report = Some(report),
                        Err(e) => return Err(ExecError::Decode(e.to_string())),
                    }
                    continue;
                }
                // Engine-injected, like `submit_report` above: handled in-process, never reaches
                // `ToolRuntime`, and — deliberately, before `call_history.push` below — never
                // enters doom-loop/cycle tracking. Legitimate scratchpad usage (update after a
                // real tool call, repeated; several updates in a row while planning) would
                // otherwise misfire both guards (see `liberado-scratchpad`'s module docs).
                if let Some(pad) = scratchpad
                    && call.name == SCRATCHPAD_TOOL
                {
                    let result = pad.apply(&call.arguments);
                    messages.push(Message::tool_result(&call.id, result));
                    continue;
                }
                let tool_span = tracing::debug_span!("tool_call", name = %call.name, id = %call.id);
                let result = async { run_tool(runtime, call).await }
                    .instrument(tool_span)
                    .await;
                call_history.push((call.name.clone(), call.arguments.clone(), result.clone()));
                messages.push(Message::tool_result(&call.id, result));

                if doom_hit.is_none() && is_doom_loop(&call_history) {
                    doom_hit = Some(call.name.clone());
                }
                if cycle_hit.is_none()
                    && let Some(cycling) = detect_short_cycle(&call_history)
                {
                    cycle_hit = Some(cycling);
                }
            }

            if let Some(report) = submitted_report {
                return Ok(Terminal::Filed(report));
            }

            // Escalations only after every tool_call_id has a result message.
            if let Some(tool_name) = doom_hit {
                match doom_guard.strike() {
                    Escalation::Nudge => {
                        tracing::warn!(turn, tool = %tool_name, "doom loop detected; nudging once");
                        messages.push(Message::user(DOOM_LOOP_NUDGE));
                        continue 'turn_loop;
                    }
                    Escalation::Remove => {
                        let removed = tool_name;
                        tools.retain(|t| t.name != removed);
                        tracing::warn!(
                            turn,
                            tool = %removed,
                            "doom loop persisted after nudge; removing the tool"
                        );
                        messages.push(Message::user(tool_removed_nudge(&removed)));
                        if !bonus_granted {
                            bonus_granted = true;
                            max_turns += DOOM_LOOP_RECOVERY_BONUS_TURNS;
                            tracing::info!(
                                max_turns,
                                "granted a one-time recovery top-up after tool removal"
                            );
                        }
                        continue 'turn_loop;
                    }
                    Escalation::GiveUp => {
                        tracing::warn!(
                            turn,
                            tool = %tool_name,
                            "doom loop persisted after tool removal; aborting"
                        );
                        return Ok(Terminal::Filed(doom_loop_failed_report(&tool_name)));
                    }
                }
            }
            if let Some(cycling) = cycle_hit {
                match cycle_guard.strike() {
                    Escalation::Nudge => {
                        tracing::warn!(turn, ?cycling, "tool cycle detected; nudging once");
                        messages.push(Message::user(CYCLE_NUDGE));
                        continue 'turn_loop;
                    }
                    Escalation::Remove => {
                        tools.retain(|t| !cycling.contains(&t.name));
                        tracing::warn!(
                            turn,
                            ?cycling,
                            "tool cycle persisted after nudge; removing the cycling tools"
                        );
                        messages.push(Message::user(tools_removed_nudge(&cycling)));
                        if !bonus_granted {
                            bonus_granted = true;
                            max_turns += DOOM_LOOP_RECOVERY_BONUS_TURNS;
                            tracing::info!(
                                max_turns,
                                "granted a one-time recovery top-up after tool removal"
                            );
                        }
                        continue 'turn_loop;
                    }
                    Escalation::GiveUp => {
                        tracing::warn!(
                            turn,
                            ?cycling,
                            "tool cycle persisted after tool removal; aborting"
                        );
                        return Ok(Terminal::Filed(cycle_failed_report()));
                    }
                }
            }
        }

        let exhausted_name = exhausted_resource.unwrap_or("turns");
        tracing::warn!(
            turns = max_turns,
            resource = exhausted_name,
            "execution budget exhausted"
        );
        match mode {
            // The delegating agent is owed a Report, not a transport error — and it deserves to
            // know what actually happened, not just that time ran out. See
            // `budget_failed_report_with_progress`'s doc comment for why this stays a compact,
            // mechanical summary rather than injecting the raw call history upward.
            Mode::Report => Ok(Terminal::Filed(budget_failed_report_with_progress(
                exhausted_name,
                max_turns,
                &call_history,
            ))),
            Mode::Conversational => Err(ExecError::BudgetExceeded { turns: max_turns }),
        }
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
        deferred_to_human: false,
        follow_up: None,
    }
}

/// The `Report` returned when the turn budget is exhausted without completion and no calls were
/// ever made to say anything about (the empty-history fallback `budget_failed_report_with_progress`
/// defers to, and the one live path — `converse_stream`'s own budget exhaustion — that has no
/// `Report` concept and can't call the enriched version at all).
fn budget_failed_report(turns: u32) -> Report {
    Report {
        outcome: Outcome::Failed,
        summary: format!("Execution exceeded the {turns}-turn budget without completing."),
        artifacts: Vec::new(),
        new_high_signal_facts: Vec::new(),
        deferred_to_human: false,
        follow_up: Some("Consider dispatching a subagent with a larger budget.".into()),
    }
}

/// The same failure, naming which *other* resource (wall-clock, tokens, ...) ran out instead of
/// the turn count, when that's what actually happened — `resource` is `"turns"` when it was the
/// plain turn cap. See `budget_failed_report_with_progress`'s doc comment for the full-history
/// version of this same naming.
fn budget_failed_report_named(resource: &str, turns: u32) -> Report {
    if resource == "turns" {
        return budget_failed_report(turns);
    }
    Report {
        outcome: Outcome::Failed,
        summary: format!("Execution exceeded its {resource} budget without completing."),
        artifacts: Vec::new(),
        new_high_signal_facts: Vec::new(),
        deferred_to_human: false,
        follow_up: Some(format!(
            "Consider raising the {resource} budget, or narrowing the goal so less {resource} is needed."
        )),
    }
}

/// The `Report` returned when the turn budget is exhausted, built from what the run actually did
/// instead of a bare "ran out of turns" — a real live gap: a model that made genuine progress
/// (e.g. wrote a vault note) before running out of turns to file `submit_report` previously
/// reported back as a bare `Failed`, `artifacts: []`, indistinguishable from a run that made no
/// progress at all. The deploying agent needs enough signal to decide "redeploy from here" vs.
/// "start over," without the raw tool-call/result trace bubbling up a layer — that would defeat the
/// token-efficiency point of delegating in the first place. So: a compact, mechanical listing
/// (tool name + a short preview of its result, reusing the same `preview()` truncation the
/// streaming path already uses for the same reason) rather than either extreme. `PartiallySucceeded`
/// when at least one call actually succeeded (not `Failed`, which would incorrectly read the same
/// as zero progress); `artifacts`/`new_high_signal_facts` are deliberately left for a human or a
/// future cheap-model summarizer to derive — mechanically guessing which preview strings are
/// "really" a written artifact path would mean parsing arbitrary tool-specific result text, which
/// is a judgment call, not a mechanical one.
fn budget_failed_report_with_progress(
    resource: &str,
    turns: u32,
    call_history: &[(String, serde_json::Value, String)],
) -> Report {
    if call_history.is_empty() {
        return budget_failed_report_named(resource, turns);
    }
    let any_succeeded = call_history
        .iter()
        .any(|(_, _, result)| !result.starts_with("tool error:"));
    let call_list = call_history
        .iter()
        .map(|(name, _, result)| format!("{name} -> {}", preview(result)))
        .collect::<Vec<_>>()
        .join("; ");
    let budget_desc = if resource == "turns" {
        format!("{turns}-turn budget")
    } else {
        format!("{resource} budget")
    };
    Report {
        outcome: if any_succeeded {
            Outcome::PartiallySucceeded
        } else {
            Outcome::Failed
        },
        summary: format!(
            "Execution exceeded its {budget_desc} before filing a report. Calls made: {call_list}."
        ),
        artifacts: Vec::new(),
        new_high_signal_facts: Vec::new(),
        deferred_to_human: false,
        follow_up: Some(
            "Some tool calls may have completed before the budget ran out (see summary) — \
             redeploying with a larger budget, or a narrower remaining goal, may be able to finish \
             from here rather than starting over."
                .into(),
        ),
    }
}

/// Whether the last [`DOOM_LOOP_THRESHOLD`] invocations are consecutively the same tool, called
/// with near-duplicate arguments (see `args_similarity`) — see [`DOOM_LOOP_THRESHOLD`]'s doc
/// comment for why near-duplicate, not just byte-identical, is the right bar.
fn is_doom_loop(history: &[(String, serde_json::Value, String)]) -> bool {
    let Some((last_name, ..)) = history.last() else {
        return false;
    };
    // Most-recent-first, stopping at the first call that isn't consecutively the same tool.
    let streak: Vec<&serde_json::Value> = history
        .iter()
        .rev()
        .take_while(|(name, ..)| name == last_name)
        .map(|(_, args, _)| args)
        .collect();
    if streak.len() < DOOM_LOOP_THRESHOLD {
        return false;
    }
    streak[..DOOM_LOOP_THRESHOLD]
        .windows(2)
        .all(|pair| args_similarity(pair[0], pair[1]) >= ARG_SIMILARITY_THRESHOLD)
}

/// Whether the tool-name sequence at the tail of `history` is a short repeating cycle (period 2 or
/// 3 — e.g. A,B,A,B or A,B,C,A,B,C). Exact tool-name match only, no argument comparison: see
/// [`CYCLE_NUDGE`]'s doc comment for why that's an acceptable, deliberately simpler bar than
/// `is_doom_loop`'s. Returns the distinct tool names participating in the cycle (so the caller can
/// remove exactly those, not the whole catalog) rather than a bare bool.
///
/// A mono-tool streak (`read_note`×4 in one parallel batch) is **not** a cycle — period-2 would
/// match `AAAA` as two copies of `AA`, which is a false positive that used to mid-batch-nudge and
/// leave unanswered `tool_call_id`s (dogfood session `01KX7BWV`). Same-tool thrash is
/// [`is_doom_loop`]'s job (args-aware).
fn detect_short_cycle(history: &[(String, serde_json::Value, String)]) -> Option<Vec<String>> {
    let names: Vec<&str> = history.iter().map(|(name, ..)| name.as_str()).collect();
    for period in 2..=3 {
        let window = period * 2;
        if names.len() < window {
            continue;
        }
        let tail = &names[names.len() - window..];
        let (first_half, second_half) = tail.split_at(period);
        if first_half != second_half {
            continue;
        }
        let mut distinct: Vec<String> = first_half.iter().map(|s| s.to_string()).collect();
        distinct.sort_unstable();
        distinct.dedup();
        // Require a real multi-tool pattern, not "the same tool N times in a row".
        if distinct.len() < 2 {
            continue;
        }
        return Some(distinct);
    }
    None
}

/// Object keys whose string values name a distinct resource. If both calls set the same key to
/// *different* strings, the calls are not near-duplicates — bag-of-words alone would still score
/// `{"path":"Tasks/A.md"}` vs `{"path":"Tasks/B.md"}` high (shared `path` / `tasks` / `md` tokens)
/// and false-positive doom-loop on legitimate parallel multi-file reads (dogfood `01KX7BWV`).
const IDENTITY_ARG_KEYS: &[&str] = &["path", "file", "filepath", "note", "uri", "id"];

/// Cosine similarity between two tool calls' arguments, weighted so a term shared by both calls
/// (boilerplate, or the topic every rephrasing shares) counts for less than a term unique to one
/// side — see [`DOOM_LOOP_THRESHOLD`]'s doc comment for why byte-equality alone missed the real
/// failure this guards against. `1.0` when both sides tokenize to nothing (e.g. both `{}`): with no
/// text to compare, equality of the raw value is the only signal left. Deterministic, local,
/// no network/model call — a small bag-of-words IDF over just the two documents being compared,
/// not a learned embedding.
///
/// Before TF-IDF: if both args carry an [`IDENTITY_ARG_KEYS`] field and the values differ, return
/// `0.0` immediately (distinct resources ⇒ not a doom loop).
fn args_similarity(a: &serde_json::Value, b: &serde_json::Value) -> f32 {
    if identity_args_conflict(a, b) {
        return 0.0;
    }
    let tokens_a = tokenize(a);
    let tokens_b = tokenize(b);
    if tokens_a.is_empty() && tokens_b.is_empty() {
        return if a == b { 1.0 } else { 0.0 };
    }
    let vectors = tf_idf_vectors(&[tokens_a, tokens_b]);
    cosine(&vectors[0], &vectors[1])
}

/// True when both objects set the same identity key to different string values.
fn identity_args_conflict(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    for key in IDENTITY_ARG_KEYS {
        match (
            a.get(*key).and_then(|v| v.as_str()),
            b.get(*key).and_then(|v| v.as_str()),
        ) {
            (Some(x), Some(y)) if x != y => return true,
            _ => {}
        }
    }
    false
}

/// Lowercased alphanumeric runs from a JSON value's textual form — deliberately crude (no
/// stemming/stopwords), adequate for comparing short tool-call argument strings.
fn tokenize(value: &serde_json::Value) -> Vec<String> {
    value
        .to_string()
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// TF-IDF vectors for a small set of tokenized documents, IDF computed from just that set (there's
/// no larger corpus available or wanted here — see `args_similarity`).
fn tf_idf_vectors(docs: &[Vec<String>]) -> Vec<std::collections::HashMap<String, f32>> {
    let n = docs.len() as f32;
    let mut doc_freq: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for doc in docs {
        let unique: std::collections::HashSet<&str> = doc.iter().map(String::as_str).collect();
        for term in unique {
            *doc_freq.entry(term).or_insert(0) += 1;
        }
    }
    docs.iter()
        .map(|doc| {
            let mut tf: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
            for term in doc {
                *tf.entry(term.clone()).or_insert(0.0) += 1.0;
            }
            for (term, count) in tf.iter_mut() {
                let df = *doc_freq.get(term.as_str()).unwrap_or(&1) as f32;
                // +1 smoothing: a term every doc shares still contributes a little (so two fully
                // identical documents still cosine to 1.0), rather than vanishing entirely.
                *count *= (n / df).ln() + 1.0;
            }
            tf
        })
        .collect()
}

fn cosine(
    a: &std::collections::HashMap<String, f32>,
    b: &std::collections::HashMap<String, f32>,
) -> f32 {
    let dot: f32 = a
        .iter()
        .map(|(term, weight)| weight * b.get(term).copied().unwrap_or(0.0))
        .sum();
    let norm_a = a.values().map(|v| v * v).sum::<f32>().sqrt();
    let norm_b = b.values().map(|v| v * v).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// The `Report` returned when the doom-loop guard fires a second time: the model kept calling the
/// same tool with near-duplicate arguments even after one corrective nudge.
fn doom_loop_failed_report(tool_name: &str) -> Report {
    Report {
        outcome: Outcome::Failed,
        summary: format!(
            "Stopped: called `{tool_name}` with near-duplicate arguments {DOOM_LOOP_THRESHOLD}+ \
             times in a row without making progress, even after a correction."
        ),
        artifacts: Vec::new(),
        new_high_signal_facts: Vec::new(),
        deferred_to_human: false,
        follow_up: Some(format!(
            "The `{tool_name}` result may not carry enough information to act on, or the goal may \
             need to be rephrased/split into smaller steps."
        )),
    }
}

/// The `Report` returned when a short tool-name cycle (see [`is_short_cycle`]) persists past one
/// corrective nudge.
fn cycle_failed_report() -> Report {
    Report {
        outcome: Outcome::Failed,
        summary: "Stopped: cycling between the same short sequence of tools without making \
                   progress, even after a correction."
            .to_string(),
        artifacts: Vec::new(),
        new_high_signal_facts: Vec::new(),
        deferred_to_human: false,
        follow_up: Some(
            "The goal may need to be rephrased/split into smaller, more concrete steps.".into(),
        ),
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
    async fn budget_exhaustion_with_no_progress_is_a_failed_report() {
        // Every call errors — genuinely no progress — so exhaustion must stay `Failed`, not
        // `PartiallySucceeded`.
        let (_provider, exec) = executor(
            vec![call_tool("search"), call_tool("search")],
            Budget::new(2),
        );
        let runtime = MockToolRuntime::new(&["search"], Err("upstream 500".into()));

        let report = exec
            .execute(&runtime, Task::new("worker", "loop forever"))
            .await
            .unwrap();

        assert_eq!(report.outcome, Outcome::Failed);
        assert!(report.summary.contains("budget"));
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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));

        let report = exec
            .execute(&runtime, Task::new("worker", "loop forever"))
            .await
            .unwrap();

        assert_eq!(report.outcome, Outcome::PartiallySucceeded);
        assert!(report.summary.contains("budget"), "{}", report.summary);
        assert!(report.summary.contains("search"), "{}", report.summary);
        assert!(report.summary.contains("3 hits"), "{}", report.summary);
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
        let runtime = MockToolRuntime::new(&["search", "other_tool"], Ok("same result".into()));

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
    async fn doom_loop_persisting_past_the_nudge_removes_the_tool_then_aborts_if_it_still_repeats()
    {
        // Escalation ladder: 1st detection (3rd identical call) nudges; 2nd (4th) removes the tool
        // from what's offered and explains why; 3rd (5th — the tool somehow got called again
        // anyway) gives up rather than burn the rest of the turn budget.
        let (provider, exec) = executor(
            vec![
                call_tool("search"),
                call_tool("search"),
                call_tool("search"),
                call_tool("search"),
                call_tool("search"),
            ],
            Budget::default(),
        );
        let runtime = MockToolRuntime::new(&["search"], Ok("same result".into()));

        let report = exec
            .execute(&runtime, Task::new("worker", "do it"))
            .await
            .unwrap();

        assert_eq!(report.outcome, Outcome::Failed);
        assert!(report.summary.contains("search"), "{}", report.summary);
        assert_eq!(runtime.invoked().len(), 5);
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
        let runtime = MockToolRuntime::new(&["search"], Ok("same result".into()));

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
                call_tool("search"), // turn 4: 2nd detection, `search` removed + bonus granted
                submit(valid_report_args()), // turn 5: only reachable because of the bonus
            ],
            Budget::new(4),
        );
        let runtime = MockToolRuntime::new(&["search"], Ok("same result".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("same result".into()));

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
        let runtime = MockToolRuntime::new(
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
        let runtime = MockToolRuntime::new(&["search"], Ok("a result".into()));

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
        let runtime = MockToolRuntime::new(&["tool-a", "tool-b"], Ok("result".into()));

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
            !is_doom_loop(&h),
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

    #[test]
    fn same_path_read_note_three_times_is_a_doom_loop() {
        // Mono-tool thrash: same tool + same args — doom-loop's job, not short-cycle.
        let path = serde_json::json!({"path": "Tasks/Sarah.md"});
        let h = hist(&[
            ("turbovault:read_note", path.clone()),
            ("turbovault:read_note", path.clone()),
            ("turbovault:read_note", path),
        ]);
        assert!(is_doom_loop(&h), "identical path ×3 must trip doom-loop");
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
        assert!(is_doom_loop(&h));
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
        assert!(!is_doom_loop(&h));
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

    #[test]
    fn two_identical_calls_are_not_yet_a_doom_loop() {
        // Threshold is 3 — two repeats is allowed (batch of two different intents might
        // still share a tool; wait for the third near-duplicate).
        let path = serde_json::json!({"path": "Tasks/Sarah.md"});
        let h = hist(&[
            ("turbovault:read_note", path.clone()),
            ("turbovault:read_note", path),
        ]);
        assert!(!is_doom_loop(&h));
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
            is_doom_loop(&h),
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
        let runtime = MockToolRuntime::new(&["tool-a", "tool-b"], Ok("result".into()));

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
        let runtime = MockToolRuntime::new(&["turbovault:read_note"], Ok("note body".into()));

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
        let runtime = MockToolRuntime::new(&["turbovault:read_note"], Ok("note body".into()));

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
        // tools and explain why (turn 5), then give up if it somehow still repeats (turn 6).
        let (provider, exec) = executor(
            vec![
                call_tool("tool-a"),
                call_tool("tool-b"),
                call_tool("tool-a"),
                call_tool("tool-b"),
                call_tool("tool-a"),
                call_tool("tool-b"),
            ],
            Budget::default(),
        );
        let runtime = MockToolRuntime::new(&["tool-a", "tool-b"], Ok("result".into()));

        let report = exec
            .execute(&runtime, Task::new("worker", "do it"))
            .await
            .unwrap();

        assert_eq!(report.outcome, Outcome::Failed);
        assert!(report.summary.contains("cycling"), "{}", report.summary);
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
        let runtime = MockToolRuntime::new(&["tool-a", "tool-b"], Ok("result".into()));

        let report = exec
            .execute(&runtime, Task::new("worker", "do it"))
            .await
            .unwrap();

        assert_eq!(report.outcome, Outcome::Succeeded);
        assert_eq!(runtime.invoked().len(), 5);
    }

    #[tokio::test]
    async fn a_doom_loop_gets_its_own_nudge_even_after_the_cycle_guard_already_struck_once() {
        // Regression for the shared-counter bug (`docs/roadmap/hygiene-audit-2026-07-05.md` P2.1):
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
        let runtime = MockToolRuntime::new(
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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("a result".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));
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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

        let report = exec
            .execute(&runtime, Task::new("worker", "do it"))
            .await
            .unwrap();

        assert_eq!(report.outcome, Outcome::Succeeded);
        // The scratchpad call never went through ToolRuntime at all.
        assert!(runtime.invoked().is_empty());
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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));
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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

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
}
