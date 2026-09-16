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
//! depends on the trait, so it is testable with a mock runtime and a `MockProvider`.

mod budget;
mod conversation_reserve;
mod loop_guard;
mod mvl;
mod risk_gated;

pub use budget::{Budget, ResourceLimit, ResourceUsage, TokenLimit, WallClockLimit};
pub use loop_guard::{ArgMatch, LoopProfile};
pub use mvl::MvlSession;
pub use risk_gated::RiskGatedToolRuntime;

use crate::loop_guard::{
    CYCLE_NUDGE, DOOM_LOOP_NUDGE, Escalation, LoopGuard, RunPolicy, detect_short_cycle,
    is_doom_loop,
};

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use liberado_common::{Outcome, Report, ToolCall};
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

fn sanitize_spill_label(label: &str) -> String {
    let mut out: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if out.is_empty() {
        out.push_str("call");
    }
    out
}

/// Path the model should `read_file`, workspace-relative when `.liberado/offload` is in use.
fn spill_preview_path(spill_dir: &std::path::Path, file_name: &str) -> String {
    if spill_dir.file_name().is_some_and(|n| n == "offload") {
        format!(".liberado/offload/{file_name}")
    } else {
        spill_dir
            .join(file_name)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

fn char_boundary_at_or_before(text: &str, mut idx: usize) -> usize {
    idx = idx.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn truncate_head(text: &str, max: usize) -> String {
    let end = char_boundary_at_or_before(text, max);
    text[..end].to_string()
}

/// Spill an oversized tool result: write the full body and return a head+tail preview.
///
/// Under the threshold, returns `text` unchanged. A second pack needs this — it lives
/// in the kernel, not in a coding-only clip.
fn spill_oversized_result(
    text: &str,
    max_bytes: usize,
    spill_dir: &std::path::Path,
    label: &str,
) -> (String, Option<String>) {
    if text.len() <= max_bytes {
        return (text.to_string(), None);
    }
    let file_name = format!("tool-spill-{}.txt", sanitize_spill_label(label));
    let path = spill_dir.join(&file_name);
    let _ = std::fs::create_dir_all(spill_dir);
    if std::fs::write(&path, text).is_err() {
        let head = truncate_head(text, 2048);
        return (
            format!("{head}\n\n··· (truncated, {} total bytes) ···", text.len()),
            None,
        );
    }
    let preview_path = spill_preview_path(spill_dir, &file_name);
    const HEAD: usize = 2048;
    const TAIL: usize = 1024;
    let head_end = char_boundary_at_or_before(text, HEAD);
    let tail_start = char_boundary_at_or_before(text, text.len().saturating_sub(TAIL));
    let preview = if head_end >= tail_start {
        format!(
            "{}\n\n··· (truncated, {} total bytes; full body at `{preview_path}`) ···",
            &text[..head_end],
            text.len()
        )
    } else {
        format!(
            "{}\n\n··· (truncated, {} total bytes; full body at `{preview_path}`) ···\n\n{}",
            &text[..head_end],
            text.len(),
            &text[tail_start..]
        )
    };
    (preview, Some(preview_path))
}

/// Name of the synthetic finish-tool the engine injects in report mode. A real [`ToolRuntime`]
/// must not expose a tool with this name (it would be shadowed by the engine's terminator).
pub const SUBMIT_REPORT_TOOL: &str = "submit_report";

/// Default turn budget. Generous enough for a multi-step subagent, bounded enough that a confused
/// model can't loop forever. `ExecuteDirect` should pass a tighter budget derived from
/// `small_fanout`.
pub const DEFAULT_MAX_TURNS: u32 = 8;

/// Turns handed back to a **salvageable** run whose budget ran out, solely so it can file what it
/// already has.
///
/// Live evidence: a deep-research subagent spent all 8 turns on ~28 successful searches and was
/// cut off before ever calling `submit_report`, so a run that had done the work returned nothing
/// but a synthesized "ran out of turns". The research was not the failure — the write-up was, and
/// the model had no way to know it was on its last turn.
///
/// This is not a budget increase. Every other tool is withdrawn when the reserve is granted, so
/// the turns cannot be spent continuing the work they were given to conclude — the same lever the
/// doom-loop guard pulls at strike 2, for the same reason: a nudge alone did not change model
/// behaviour in live testing. Granted at most once, so worst-case turns stay bounded by
/// `max_turns + DOOM_LOOP_RECOVERY_BONUS_TURNS + WRAP_UP_TURNS`.
pub const WRAP_UP_TURNS: u32 = 3;

/// One tool-free turn reserved for a conversational agent to tell the human what happened after
/// it spends the configured work budget. This prevents a successful final tool call from ending as
/// a transport error with no reply, while still forbidding more work beyond the ceiling.
pub const CONVERSATION_WRAP_UP_TURNS: u32 = 1;

/// Appended once if the model answers in prose without filing a `Report`. Deliberately offers
/// *both* options (keep going, or finish) rather than unconditionally pushing to wrap up — an
/// earlier wording ("Before finishing, call `submit_report`...") biased a model that paused to
/// narrate mid-plan toward prematurely filing instead of continuing a genuinely multi-step goal, a
/// real live finding from `liberado-heuristics-tuner`'s executor-layer tuning (a scenario needing
/// two distinct tool calls scored 0/6 across two independent runs, even under system prompts that
/// explicitly instructed both calls — the nudge's own wording was working against the prompt at
/// exactly the moment it mattered, docs/future-work/heuristics-tuning-engine-plan.md).
/// How many times a run will hand a malformed `submit_report` back to the model before giving up.
///
/// Two, because the failure it guards is a schema slip the model corrects on being told (a missing
/// `outcome` field), not a capability gap — and a model that has not produced the right shape twice
/// will not find it on a third try. Every other tool error is already fed back in-band; this makes
/// `submit_report` consistent with them instead of uniquely fatal.
const MAX_MALFORMED_REPORTS: u32 = 2;

const REPORT_NUDGE: &str = "If the goal isn't finished yet, continue by calling whatever tool you \
still need — don't stop partway through a multi-step plan. Once it's actually done (or you \
genuinely cannot proceed), call `submit_report` with your final result. Do not reply in plain text.";

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

// The recovery top-up is per-run, not per-mechanism, so it stays here next to
// WRAP_UP_TURNS rather than with the per-mechanism guard.

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

/// Said when a model keeps calling a tool that has already been withdrawn.
///
/// Withdrawing a tool only changes the catalog the model is *shown*; it can still name one it
/// remembers from an earlier turn, and models do. That used to end the run outright — which is a
/// severe response to a model that is otherwise working: a live coding run had edited ten files
/// across six crates when it re-read one test file once too often, and the abort threw the attempt
/// away, `outcome=Failed`, before it ever reached `validate` to discover it had left a syntax error
/// behind. Refusing the call costs a turn. Ending the run costs everything the run had done.
///
/// The budget remains the real bound: a model that ignores this simply runs out of turns, and that
/// path already files a report rather than discarding the work.
fn tool_withdrawn_refusal(tool_name: &str) -> String {
    format!(
        "`{tool_name}` is withdrawn and every further call to it will be refused — repeating it \
         cannot return anything new. You still have your other tools. Finish the work with those \
         (including any verification step available to you), or call `submit_report` describing \
         what you completed and what remains."
    )
}

/// The second escalation step for a persisting tool-cycle — see [`tool_removed_nudge`]'s doc
/// comment for why the model is told, not just silently restricted.
/// Told to a salvageable run when its budget runs out and the wrap-up reserve is granted.
///
/// States the withdrawal as fact rather than asking for restraint: the tools really are gone by
/// the time this is read, so the model is not being asked to resist a temptation it still has.
fn wrap_up_directive(resource: &str, reserve: u32) -> String {
    format!(
        "You have run out of {resource}. Every tool except `{SUBMIT_REPORT_TOOL}` has been \
         withdrawn, and you have {reserve} turn(s) left.\n\n\
         Do not start new work — there is no longer any way to. Call `{SUBMIT_REPORT_TOOL}` now \
         with what you already have: the findings gathered so far, and a plain statement of what \
         you did not get to. Set `outcome` to `PartiallySucceeded`, or `Failed` if nothing useful \
         was gathered. An incomplete report is worth far more to the caller than none."
    )
}

fn conversation_wrap_up_directive(resource: &str) -> String {
    format!(
        "You have run out of {resource}. All tools are now withdrawn. Reply to the human now with \
         a concise, honest summary of what succeeded, what failed, and what remains. Do not call \
         another tool."
    )
}

fn tools_removed_nudge(tool_names: &[String]) -> String {
    let list = tool_names.join("`, `");
    format!(
        "The `{list}` tool(s) have been removed for the rest of this task — cycling between them \
         wasn't making progress. Use what you already have with your remaining tools, or call \
         `submit_report` if nothing else can move the goal forward."
    )
}

// The tool-runtime contract lives in `liberado-tool-runtime` (foundation), sunk below both the
// engine and the MCP adapter so every consumer — including the shared test doubles — implements
// the same trait instance. Re-exported here so `liberado_executor::ToolRuntime` (and friends)
// keep naming the same items.
pub use liberado_tool_runtime::{RuntimeFactory, RuntimeSetupError, ToolRuntime};

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
    /// Is half-finished work still worth returning?
    ///
    /// True for gathering tasks — research, summarisation, review — where partial findings have
    /// real value and nothing was left mutated. False (the default) for work whose deliverable is
    /// all-or-nothing: a half-applied refactor or a partially written file is not a smaller
    /// success, it is a mess, and reporting it as partial credit would misrepresent the state.
    ///
    /// Only affects what happens at budget exhaustion — a salvageable run gets
    /// [`WRAP_UP_TURNS`] to file what it has; everything else fails exactly as before.
    pub salvageable: bool,
    /// How strictly repeated tool calls are judged — see [`LoopProfile`].
    pub loop_profile: LoopProfile,
}

impl Task {
    pub fn new(instructions: impl Into<String>, goal: impl Into<String>) -> Self {
        Self {
            instructions: instructions.into(),
            goal: goal.into(),
            seed_calls: Vec::new(),
            salvageable: false,
            loop_profile: LoopProfile::default(),
        }
    }

    /// Seed the loop with an opening move (the classifier's pre-planned first calls).
    pub fn with_seed(mut self, seed_calls: Vec<ToolCall>) -> Self {
        self.seed_calls = seed_calls;
        self
    }

    /// Mark partial results worth returning — see [`Task::salvageable`].
    pub fn salvageable(mut self, salvageable: bool) -> Self {
        self.salvageable = salvageable;
        self
    }

    /// Choose how strictly repeated tool calls are judged — see [`LoopProfile`].
    pub fn loop_profile(mut self, profile: LoopProfile) -> Self {
        self.loop_profile = profile;
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
    #[error("execution exceeded its {resource} budget after {turns} turn(s)")]
    /// `resource` is the bound that actually ran out — `"turns"`, `"wall-clock"`, `"tokens"`.
    /// Carried because the catch site files a report, and a report that always blames turns
    /// misdirects whoever reads it, model or human.
    BudgetExceeded { resource: &'static str, turns: u32 },
    #[error("internal executor invariant violated: {0}")]
    Internal(&'static str),
    /// Conversational loop stopped because a tool asked the human. The assistant
    /// tool-call is already in `messages`; do not append a tool result until the
    /// next user message arrives, then resume.
    #[error("awaiting a human answer for tool call {call_id}")]
    AwaitingHuman { call_id: String },
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

/// What the model was sent on one turn, and what it sent back.
///
/// Exists because none of it was recorded anywhere. Diagnosing a coding run meant re-deriving the
/// tool catalog from `catalog()` and `PathPolicy` by hand, and the model's own account of why it
/// stopped — "blocked from making edits by the progress guard" — was invisible through four
/// consecutive failed runs until someone happened to attach `RUST_LOG=liberado_executor=debug` to
/// a live one. A turn is the unit at which that becomes answerable.
#[derive(Debug, Clone)]
pub struct TurnRecord {
    pub turn: u32,
    /// Names of the tools **offered** on this turn, in catalog order. Guards withdraw tools as a
    /// run proceeds, so this changes turn to turn and is the only record of what the model could
    /// actually reach when it made its choice.
    pub tools_offered: Vec<String>,
    /// How many messages the model was sent — conversation depth, without the payload.
    pub message_count: usize,
    /// The model's own text for this turn, verbatim and untruncated. `None` when it emitted only
    /// tool calls.
    pub content: Option<String>,
    /// `"tool_calls"` or `"prose"` — why the turn ended, in the loop's own vocabulary.
    pub finish_reason: &'static str,
    /// Tool names the model asked for, in call order.
    pub tool_calls: Vec<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// What the model was **sent**, captured before the call rather than after it.
///
/// [`TurnRecord`] is emitted once a turn completes, so everything it holds is a fact about the
/// response. Which system prompt actually reached the model — whether a role's inline `prompt` or
/// its `prompt_path` won, and what text that produced — appeared in no trace at all.
///
/// That gap is not theoretical. Comparing this harness against another on the same task, the
/// remaining unexplained difference was what each one told the model, and neither side's trace
/// recorded it. The measurement that would have answered it was the task being measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRecord {
    pub turn: u32,
    /// Tools offered on this request, in catalog order.
    pub tools_offered: Vec<String>,
    /// How many messages the request carried.
    pub message_count: usize,
    /// Lowercase hex SHA-256 of the system message as sent.
    ///
    /// A hash answers "did the prompt change mid-run", which is the question a long run raises.
    /// It cannot answer "what did it say" — see `system_prompt`.
    pub system_prompt_sha256: String,
    /// The system message verbatim.
    ///
    /// Carried on every request and left to the observer to store sparingly. The spec for this
    /// work asked only for the hash; a hash tells you the prompt changed and not what it says,
    /// and reading what it says is the entire reason the gap was noticed. The pack records the
    /// text once per distinct hash and the hash every turn, which is the same information at a
    /// fraction of the size — but that is a policy decision, so it lives with the pack rather
    /// than here.
    pub system_prompt: Option<String>,
}

/// Optional last look at a well-formed [`Report`] before the loop ends.
///
/// The engine already accepts Partial / Failed / wrap-up without asking, and it never reverts
/// disk work when a report is refused or the turn budget runs out. A gate may only refuse
/// `outcome=succeeded` while the model still has tools: the refusal is a tool result, the
/// conversation continues, and the worktree stays as the model left it.
///
/// Domain packs supply the check (a compile gate, a schema check). The executor stays
/// domain-neutral and does not call this when the report is already honest-and-terminal.
#[async_trait]
pub trait ReportGate: Send + Sync {
    /// `Ok(())` ends the loop with this report. `Err(message)` is handed back as the
    /// `submit_report` tool result; the model keeps the same conversation.
    async fn accept(&self, report: &Report, wrapping_up: bool) -> Result<(), String>;
}

/// Receives a [`TurnRecord`] per completed turn.
///
/// Deliberately domain-neutral: the executor knows nothing about coding sessions, and the coding
/// pack adapts these into its own trace vocabulary. Implementations must not block — they run
/// inline in the turn loop.
pub trait TurnObserver: Send + Sync {
    fn on_turn(&self, record: TurnRecord);

    /// Receives a [`RequestRecord`] **before** each model call.
    ///
    /// Defaulted to a no-op so every existing implementor keeps compiling — an observer that only
    /// cares about responses should not have to say so.
    fn on_request(&self, _record: RequestRecord) {}
}

/// The bounded, adaptive tool-loop engine. Cheap to clone-share via the inner `Arc`.
#[derive(Clone)]
pub struct Executor {
    provider: Arc<dyn Provider>,
    budget: Budget,
    /// Optional per-turn observer. `None` costs nothing and keeps every existing caller unchanged.
    observer: Option<Arc<dyn TurnObserver>>,
    /// Optional same-session check on `outcome=succeeded`. `None` accepts every well-formed report.
    report_gate: Option<Arc<dyn ReportGate>>,
    /// Production MVL / execution JSONL (backlog 0.6). `None` writes nothing.
    mvl: Option<Arc<MvlSession>>,
    /// Directory where oversized tool results are written. `None` leaves results intact.
    spill_dir: Option<PathBuf>,
    /// Byte threshold for spilling a tool result. Default 64 KiB.
    spill_max_bytes: usize,
    /// Model for the calls this executor makes. `None` = the provider's own.
    ///
    /// Held here rather than on the provider because a provider is shared by every session, and a
    /// session profile naming a model must not change the model under everyone else. `Executor` is
    /// `Clone` over two cheap fields, so a caller specialises one per turn.
    model: Option<String>,
}

/// The correction text handed back when `submit_report` arguments do not match the Report
/// schema: the model gets the error and a bound number of retries (see
/// [`MAX_MALFORMED_REPORTS`]), since a model that cannot produce the shape will not discover it
/// by repetition.
fn malformed_report_nudge(e: &serde_json::Error) -> String {
    format!(
        "`{SUBMIT_REPORT_TOOL}` was NOT accepted — your arguments did not match the required \
         schema: {e}. Call it again with the full object: `outcome` (one of \
         succeeded/partially_succeeded/failed) and `summary` are both required. Nothing else \
         about your work is lost; only this call needs redoing."
    )
}

/// The 1st rung of a doom-loop escalation: warn once and push the nudge directive.
fn doom_nudge(turn: u32, messages: &mut Vec<Message>) {
    tracing::warn!(turn, "doom loop detected; nudging once");
    messages.push(Message::user(DOOM_LOOP_NUDGE));
}

/// The 2nd rung of a doom-loop escalation: remove the offending tool so the next escalation
/// changes what's *possible*, not just what's *said*, and grant the one-time recovery top-up.
fn doom_remove(
    turn: u32,
    tools: &mut Vec<ToolDef>,
    messages: &mut Vec<Message>,
    bonus_granted: &mut bool,
    max_turns: &mut u32,
    tool_name: &str,
) {
    tools.retain(|t| t.name != tool_name);
    tracing::warn!(
        turn,
        tool = %tool_name,
        "doom loop persisted after nudge; removing the tool"
    );
    messages.push(Message::user(tool_removed_nudge(tool_name)));
    grant_recovery_bonus(bonus_granted, max_turns);
}

/// The 3rd+ rung of a doom-loop escalation: refuse the call, keep the run. See
/// `tool_withdrawn_refusal`.
fn doom_give_up(turn: u32, tools: &mut Vec<ToolDef>, messages: &mut Vec<Message>, tool_name: &str) {
    tools.retain(|t| t.name != tool_name);
    tracing::warn!(
        turn,
        tool = %tool_name,
        "doom loop persisted after tool removal; refusing the call and continuing"
    );
    messages.push(Message::user(tool_withdrawn_refusal(tool_name)));
}

/// The 1st rung of a tool-cycle escalation: warn once and push the nudge directive.
fn cycle_nudge(turn: u32, messages: &mut Vec<Message>, cycling: &[String]) {
    tracing::warn!(turn, ?cycling, "tool cycle detected; nudging once");
    messages.push(Message::user(CYCLE_NUDGE));
}

/// The 2nd rung of a tool-cycle escalation: remove the cycling tools and grant the one-time
/// recovery top-up.
fn cycle_remove(
    turn: u32,
    tools: &mut Vec<ToolDef>,
    messages: &mut Vec<Message>,
    bonus_granted: &mut bool,
    max_turns: &mut u32,
    cycling: &[String],
) {
    tools.retain(|t| !cycling.contains(&t.name));
    tracing::warn!(
        turn,
        ?cycling,
        "tool cycle persisted after nudge; removing the cycling tools"
    );
    messages.push(Message::user(tools_removed_nudge(cycling)));
    grant_recovery_bonus(bonus_granted, max_turns);
}

/// The 3rd+ rung of a tool-cycle escalation: refuse the calls, keep the run. See
/// `tool_withdrawn_refusal`.
fn cycle_give_up(
    turn: u32,
    tools: &mut Vec<ToolDef>,
    messages: &mut Vec<Message>,
    cycling: &[String],
) {
    tools.retain(|t| !cycling.contains(&t.name));
    tracing::warn!(
        turn,
        ?cycling,
        "tool cycle persisted after tool removal; refusing the calls and continuing"
    );
    messages.push(Message::user(tool_withdrawn_refusal(&cycling.join("`, `"))));
}

/// The model's reasoning shown alongside its tool call(s), if any.
fn log_reasoning_if_any(turn: u32, response: &CompletionResponse) {
    if let Some(content) = &response.content
        && !content.is_empty()
    {
        tracing::info!(turn, %content, "model's reasoning alongside the tool call(s)");
    }
}

/// The terminal outcome when the turn budget runs out. The delegating agent is owed a Report, not
/// a transport error — and it deserves to know what actually happened, not just that time ran out.
/// See `budget_failed_report_with_progress`'s doc comment for why this stays a compact, mechanical
/// summary rather than injecting the raw call history upward. `exhausted_name` is the resource
/// that ran out, so a wall-clock or token exhaustion is not misreported as "exceeded the N-turn
/// budget".
fn budget_exhausted_outcome(
    exhausted_name: &'static str,
    max_turns: u32,
    mode: Mode,
    call_history: &[(String, serde_json::Value, String)],
    repeat_calls: usize,
) -> Result<Terminal, ExecError> {
    tracing::warn!(
        turns = max_turns,
        resource = exhausted_name,
        "execution budget exhausted"
    );
    match mode {
        Mode::Report => Ok(Terminal::Filed(
            budget_failed_report_with_progress(exhausted_name, max_turns, call_history)
                .with_repeat_calls(repeat_calls),
        )),
        Mode::Conversational => Err(ExecError::BudgetExceeded {
            resource: exhausted_name,
            turns: max_turns,
        }),
    }
}

/// The one-time recovery top-up granted when a guard escalates to tool removal (see
/// `DOOM_LOOP_RECOVERY_BONUS_TURNS`). Capped to once per run by `bonus_granted`; distinct from
/// `usage`/`extra_limits`: the turn cap is the loop's own mechanical bound, the extra limits are
/// additional independently-checked bounds layered on top.
fn grant_recovery_bonus(bonus_granted: &mut bool, max_turns: &mut u32) {
    if !*bonus_granted {
        *bonus_granted = true;
        *max_turns += DOOM_LOOP_RECOVERY_BONUS_TURNS;
        tracing::info!(
            max_turns = *max_turns,
            "granted a one-time recovery top-up after tool removal"
        );
    }
}

/// Core escalation logic shared by doom-loop and tool-cycle escalations.
/// `nudge_fn` receives (turn, messages).
/// `remove_fn` receives (turn, tools, messages, bonus_granted, max_turns).
/// `give_up_fn` receives (turn, tools, messages).
#[allow(clippy::too_many_arguments)]
fn escalate<F1, F2, F3>(
    turn: u32,
    guard: &mut LoopGuard,
    tools: &mut Vec<ToolDef>,
    messages: &mut Vec<Message>,
    bonus_granted: &mut bool,
    max_turns: &mut u32,
    nudge_fn: F1,
    remove_fn: F2,
    give_up_fn: F3,
) where
    F1: FnOnce(u32, &mut Vec<Message>),
    F2: FnOnce(u32, &mut Vec<ToolDef>, &mut Vec<Message>, &mut bool, &mut u32),
    F3: FnOnce(u32, &mut Vec<ToolDef>, &mut Vec<Message>),
{
    match guard.strike() {
        Escalation::Nudge => nudge_fn(turn, messages),
        Escalation::Remove => remove_fn(turn, tools, messages, bonus_granted, max_turns),
        Escalation::GiveUp => give_up_fn(turn, tools, messages),
    }
}

/// Escalate a doom-loop detection one rung of the ladder (see [`LoopGuard`]'s doc comment for why
/// the two guards must NOT share a counter): 1st detection -> nudge, 2nd -> remove the offending
/// tool(s) and explain why, 3rd+ -> give up honestly. Removal (not just another nudge) is the
/// second step because a nudge alone did not change DeepSeek/Gemini's behavior in live testing —
/// they repeated anyway — so the next escalation needs to change what's *possible*, not just
/// what's *said*. The caller continues the loop after every rung.
fn escalate_doom(
    turn: u32,
    guard: &mut LoopGuard,
    tools: &mut Vec<ToolDef>,
    messages: &mut Vec<Message>,
    bonus_granted: &mut bool,
    max_turns: &mut u32,
    tool_name: &str,
) {
    escalate(
        turn,
        guard,
        tools,
        messages,
        bonus_granted,
        max_turns,
        doom_nudge,
        |t, tools, m, b, mx| doom_remove(t, tools, m, b, mx, tool_name),
        |t, tools, m| doom_give_up(t, tools, m, tool_name),
    )
}

/// Escalate a short-cycle detection one rung of the ladder (see [`LoopGuard`]'s doc comment).
fn escalate_cycle(
    turn: u32,
    guard: &mut LoopGuard,
    tools: &mut Vec<ToolDef>,
    messages: &mut Vec<Message>,
    bonus_granted: &mut bool,
    max_turns: &mut u32,
    cycling: &[String],
) {
    escalate(
        turn,
        guard,
        tools,
        messages,
        bonus_granted,
        max_turns,
        |t, m| cycle_nudge(t, m, cycling),
        |t, tools, m, b, mx| cycle_remove(t, tools, m, b, mx, cycling),
        |t, tools, m| cycle_give_up(t, tools, m, cycling),
    )
}

impl Executor {
    pub fn new(provider: Arc<dyn Provider>, budget: Budget) -> Self {
        Self {
            provider,
            budget,
            model: None,
            observer: None,
            report_gate: None,
            mvl: None,
            spill_dir: None,
            spill_max_bytes: 64 * 1024,
        }
    }

    /// Attach a spill directory for oversized tool results.
    ///
    /// When set, any tool result over `spill_max_bytes` is written to a file and the
    /// model sees a head+tail preview plus a `read_file` path. When unset, results
    /// pass through unchanged — other packs keep their current behaviour.
    #[must_use]
    pub fn with_spill_dir(mut self, dir: PathBuf) -> Self {
        self.spill_dir = Some(dir);
        self
    }

    /// Override the spill threshold (default 64 KiB).
    #[must_use]
    pub fn with_spill_max_bytes(mut self, max: usize) -> Self {
        self.spill_max_bytes = max;
        self
    }

    /// Attach a production MVL session. Events are append-flushed at the request/tool boundary.
    #[must_use]
    pub fn with_mvl(mut self, mvl: Arc<MvlSession>) -> Self {
        self.mvl = Some(mvl);
        self
    }

    /// Hand the observer what is about to be sent. No-op when unobserved.
    ///
    /// Called before the provider, so a run that dies mid-call still records what it asked for —
    /// which is exactly the case where knowing matters most.
    fn observe_request(
        &self,
        turn: u32,
        tools_offered: &[String],
        message_count: usize,
        messages: &[Message],
    ) {
        let Some(observer) = self.observer.as_ref() else {
            return;
        };
        let system: String = messages
            .iter()
            .find(|m| m.role == Role::System)
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let digest = <sha2::Sha256 as sha2::Digest>::digest(system.as_bytes());
        observer.on_request(RequestRecord {
            turn,
            tools_offered: tools_offered.to_vec(),
            message_count,
            system_prompt_sha256: format!("{digest:x}"),
            system_prompt: (!system.is_empty()).then_some(system),
        });
    }

    /// Hand one completed turn to the observer, if any. No-op when unobserved.
    #[allow(clippy::too_many_arguments)]
    fn observe_turn(
        &self,
        turn: u32,
        tools_offered: &[String],
        message_count: usize,
        content: Option<&str>,
        finish_reason: &'static str,
        tool_calls: &[String],
        usage: &(u32, u32),
    ) {
        let Some(observer) = self.observer.as_ref() else {
            return;
        };
        observer.on_turn(TurnRecord {
            turn,
            tools_offered: tools_offered.to_vec(),
            message_count,
            content: content.filter(|t| !t.is_empty()).map(str::to_string),
            finish_reason,
            tool_calls: tool_calls.to_vec(),
            prompt_tokens: usage.0,
            completion_tokens: usage.1,
        });
    }

    /// Attach a per-turn observer. See [`TurnRecord`] for why this exists.
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn TurnObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Attach a same-session check on `outcome=succeeded`. See [`ReportGate`].
    #[must_use]
    pub fn with_report_gate(mut self, gate: Arc<dyn ReportGate>) -> Self {
        self.report_gate = Some(gate);
        self
    }

    /// Whether a well-formed report may end the loop without asking [`ReportGate`].
    ///
    /// Partial, Failed, Proposed, and wrap-up are already honest-and-terminal: the files stay,
    /// and refusing them would either trap a model that can no longer edit or throw away the
    /// only record of half-finished work. Only a live `succeeded` can be a lie.
    pub fn report_ends_without_gate(outcome: Outcome, wrapping_up: bool) -> bool {
        wrapping_up || !matches!(outcome, Outcome::Succeeded)
    }

    fn mvl_start(&self, task: Option<&str>) {
        if let Some(mvl) = &self.mvl {
            mvl.start_run(&self.active_model(), "liberado", task);
        }
    }

    fn mvl_end(&self, outcome: &str, reason: &str) {
        if let Some(mvl) = &self.mvl {
            mvl.end_run(outcome, reason);
        }
    }

    fn mvl_request(&self, turn: u32, request: &CompletionRequest) {
        if let Some(mvl) = &self.mvl {
            mvl.on_request(i64::from(turn.saturating_sub(1)), request);
        }
    }

    fn mvl_completion(&self, turn: u32, response: &CompletionResponse) {
        if let Some(mvl) = &self.mvl {
            mvl.on_completion(i64::from(turn.saturating_sub(1)), response);
        }
    }

    fn mvl_tool_started(&self, turn: u32, call: &liberado_provider::ToolInvocation) {
        if let Some(mvl) = &self.mvl {
            mvl.on_tool_started(i64::from(turn.saturating_sub(1)), call);
        }
    }

    fn mvl_tool_result(
        &self,
        turn: u32,
        call: &liberado_provider::ToolInvocation,
        ok: bool,
        content: &str,
    ) {
        if let Some(mvl) = &self.mvl {
            mvl.on_tool_result(i64::from(turn.saturating_sub(1)), call, ok, content);
        }
    }

    /// A copy of this executor that runs its calls on `model`. `None` returns an equivalent
    /// executor, so a caller can pass a session's setting through without branching.
    #[must_use]
    pub fn with_model(&self, model: Option<String>) -> Self {
        Self {
            model,
            ..self.clone()
        }
    }

    /// The model this executor's calls will actually run on — the override when set, else the
    /// provider's. Used for logging, so a span never names a model the request did not use.
    pub fn active_model(&self) -> String {
        self.model.clone().unwrap_or_else(|| self.provider.model())
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
            model = %self.active_model(),
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
                    tracing::info!(
                        summary = %report.summary,
                        repeat_calls = report.repeat_calls,
                        "execution filed report"
                    );
                    Ok(report)
                }
                Ok(Terminal::Spoke(_)) => {
                    tracing::Span::current().record("outcome", "internal_error");
                    Err(ExecError::Internal("report mode returned prose"))
                }
                Err(ExecError::BudgetExceeded { resource, turns }) => {
                    tracing::Span::current().record("outcome", "budget_exceeded");
                    tracing::warn!(
                        turns,
                        resource,
                        "execution budget exceeded; returning failed report"
                    );
                    // `_named` falls back to the turn wording when `resource` is "turns", so this
                    // is a strict improvement rather than a second phrasing to keep in sync.
                    Ok(budget_failed_report_named(resource, turns))
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
            model = %self.active_model(),
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
        self.mvl_start(Some(&task.goal));
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

        let result = self
            .run_loop(
                runtime,
                &mut messages,
                &mut tools,
                mode,
                &mut scratchpad,
                RunPolicy {
                    salvageable: task.salvageable,
                    loop_profile: task.loop_profile,
                },
            )
            .await;
        match &result {
            Ok(Terminal::Filed(report)) => {
                let outcome = match report.outcome {
                    Outcome::Succeeded => "succeeded",
                    Outcome::PartiallySucceeded => "succeeded",
                    Outcome::Failed => "failed",
                    Outcome::Proposed => "succeeded",
                };
                self.mvl_end(outcome, &report.summary);
            }
            Ok(Terminal::Spoke(_)) => self.mvl_end("succeeded", "model finished"),
            Err(error) => self.mvl_end("aborted", &error.to_string()),
        }
        result
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
            model = %self.active_model(),
            budget = self.budget.max_turns,
        );
        async {
            let mut tools = runtime.catalog();
            // Conversational mode gets no scratchpad this pass (see liberado-scratchpad's module
            // docs) — the call site is ready for it, just not enabled yet.
            let mut scratchpad: Option<Scratchpad> = None;
            self.mvl_start(None);
            let result = self
                .run_loop(
                    runtime,
                    messages,
                    &mut tools,
                    Mode::Conversational,
                    &mut scratchpad,
                    // Never salvageable: there is no report to file early, and the human on the
                    // other end gets whatever prose the loop produced either way.
                    RunPolicy::default(),
                )
                .await;
            match &result {
                Ok(Terminal::Spoke(_)) => self.mvl_end("succeeded", "model finished"),
                Ok(Terminal::Filed(_)) => {
                    self.mvl_end("aborted", "conversational mode filed a report")
                }
                Err(error) => self.mvl_end("aborted", &error.to_string()),
            }
            match result? {
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
            model = %self.active_model(),
            budget = self.budget.max_turns,
        );
        let _enter = span.enter();
        tracing::debug!(model = %self.active_model(), "starting conversational stream turn");
        let mut tools = runtime.catalog();
        self.mvl_start(None);
        let result = async {
            let final_turn = conversation_reserve::conversation_final_turn(self.budget.max_turns);
            for turn in 1..=final_turn {
                conversation_reserve::enter_if_exhausted(
                    turn,
                    self.budget.max_turns,
                    &mut tools,
                    messages,
                );
                let request = CompletionRequest::new(messages.clone())
                    .with_tools(tools.clone())
                    .with_model(self.model.clone());
                self.mvl_request(turn, &request);
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
                self.mvl_completion(turn, &response);

                messages.push(assistant_turn(&response));
                if self
                    .finish_stream_turn(turn, runtime, &response, events, messages)
                    .await?
                {
                    return Ok(());
                }
            }

            // This loop ends only by running out of turns; the extra limits are checked in
            // `run_loop`, which names whichever of them fired.
            self.mvl_end("failed", "budget exceeded");
            Err(ExecError::BudgetExceeded {
                resource: "turns",
                turns: self.budget.max_turns,
            })
        }
        .await;
        if let Err(error) = &result
            && !matches!(
                error,
                ExecError::BudgetExceeded { .. } | ExecError::AwaitingHuman { .. }
            )
        {
            self.mvl_end("aborted", &error.to_string());
        }
        result
    }

    /// Run one streamed tool call. `Some(call_id)` means the tool parked for a human
    /// and the result must *not* be appended until the next message.
    async fn invoke_stream_tool(
        &self,
        runtime: &dyn ToolRuntime,
        call: &ToolInvocation,
        turn: u32,
        events: &Sender<AgentEvent>,
        messages: &mut Vec<Message>,
    ) -> Result<Option<String>, ExecError> {
        let _ = events
            .send(AgentEvent::ToolStarted {
                name: call.name.clone(),
                args: preview(&call.arguments.to_string()),
            })
            .await;
        self.mvl_tool_started(turn, call);
        let (ok, result) = match runtime.invoke(call).await {
            Ok(content) => (true, content),
            Err(message) => (false, format!("tool error: {message}")),
        };
        let shown = run_tool_spill(
            &result,
            self.spill_dir.as_deref(),
            self.spill_max_bytes,
            &call.id,
        );
        let _ = events
            .send(AgentEvent::ToolFinished {
                name: call.name.clone(),
                ok,
                preview: preview(&shown),
            })
            .await;
        if runtime.parks_for_human(&call.name) {
            self.mvl_end("awaiting_human", &call.id);
            return Ok(Some(call.id.clone()));
        }
        self.mvl_tool_result(turn, call, ok, &result);
        messages.push(Message::tool_result(&call.id, shown));
        Ok(None)
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
        policy: RunPolicy,
    ) -> Result<Terminal, ExecError> {
        let mut nudged = false;
        // (tool name, arguments, result) of every real invocation, in call order, across the whole
        // run — not just within one turn, since the doom loop this guards against spans turns (see
        // `DOOM_LOOP_THRESHOLD`'s doc comment). The result rides along too so a budget-exhaustion
        // failure report can show what actually happened instead of a bare "ran out of turns" —
        // see `budget_failed_report_with_progress`.
        let mut call_history: Vec<(String, serde_json::Value, String)> = Vec::new();
        // How many tool calls were byte-exact repeats of an earlier one in this run (same tool name,
        // same serialised arguments). Tallying at the tool boundary rather than at call time so the
        // count survives every exit path (including the guard escalations) without extra plumbing.
        let mut repeat_calls: usize = 0;
        // Malformed `submit_report` arguments are handed back to the model for correction rather
        // than aborting the run; run-scoped so the bound is total, not per turn.
        let mut malformed_reports: u32 = 0;
        // How much of `repeat_calls` has already been journaled, so each event carries only its own
        // share (see the delta comment at the completion call below).
        let mut reported_repeats: usize = 0;
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
        // Set once the wrap-up reserve is granted (see `WRAP_UP_TURNS`). Also latches the reserve
        // to one grant: the second exhaustion ends the run for real.
        let mut wrapping_up = false;
        let mut turn: u32 = 0;
        let mut usage = ResourceUsage::default();
        let run_started = liberado_common::clock::now();
        // The loop yields the name of whatever ran out, so the exhaustion report below can say
        // which bound was hit rather than always blaming turns.
        let exhausted_name: &'static str = 'turn_loop: loop {
            turn += 1;
            usage.turns = turn;
            // Both ends on the same clock. `run_started.elapsed()` measures against the real
            // `Instant::now()` regardless of `clock::now()`, so the start was injectable and
            // the end was not — freezing the clock moved neither, and no test could reach a
            // non-zero wall-clock exhaustion. A half-injected timer is worse than none: it
            // looks controllable and silently is not.
            usage.elapsed = liberado_common::clock::now().duration_since(run_started);
            // Once the reserve is running, only its own turn cap applies. The extra limits are
            // spent by definition at that point, so re-checking them would end the reserve on its
            // first turn and it could never be used — the same trap that made the doom-loop
            // guard's tool removal structurally useless before it got a top-up.
            if let Some(name) = self.exhaustion_step(
                turn,
                &usage,
                mode,
                &policy,
                &mut wrapping_up,
                &mut max_turns,
                tools,
                messages,
            ) {
                break 'turn_loop name;
            }
            let turn_span = tracing::debug_span!(
                "turn",
                turn,
                tool_calls = tracing::field::Empty,
                finish_reason = tracing::field::Empty,
            );
            // Snapshot what this turn was actually offered. Guards withdraw tools mid-run, so
            // this differs turn to turn and is the only record of what the model could reach.
            let offered: Vec<String> = self
                .observer
                .as_ref()
                .map(|_| tools.iter().map(|t| t.name.clone()).collect())
                .unwrap_or_default();
            let sent_messages = messages.len();
            self.observe_request(turn, &offered, sent_messages, &messages[..]);
            let request = CompletionRequest::new(messages.clone())
                .with_tools(tools.clone())
                .with_model(self.model.clone());
            self.mvl_request(turn, &request);
            let response = async {
                // The **delta** since the previous completion, not the running total. Every
                // numeric field on a `LatencyEvent` is additive — the cost rollup sums them — so
                // journaling a monotonically rising counter makes a run with N repeats roll up as
                // 1+2+…+N. Deltas sum to N, compose across the multiple runs that share one
                // correlation, and match how `prompt_tokens` and friends already behave.
                let delta = repeat_calls - reported_repeats;
                reported_repeats = repeat_calls;
                liberado_provider::latency::with_repeat_calls(
                    delta,
                    self.provider.complete(request),
                )
                .await
            }
            .instrument(tracing::debug_span!("provider_complete", turn))
            .await?;
            self.mvl_completion(turn, &response);

            let usage_delta = response
                .usage
                .as_ref()
                .map(|u| (u.prompt_tokens, u.completion_tokens))
                .unwrap_or((0, 0));
            if let Some(response_usage) = &response.usage {
                usage.tokens += u64::from(response_usage.total_tokens);
            }

            // Record the model's turn (content and/or tool calls) so it sees its own history.
            messages.push(assistant_turn(&response));

            if response.tool_calls.is_empty() {
                if let Some(terminal) = self.handle_prose(
                    turn,
                    &offered,
                    sent_messages,
                    &response,
                    mode,
                    &mut nudged,
                    messages,
                    &usage_delta,
                    repeat_calls,
                    &turn_span,
                ) {
                    return Ok(terminal);
                }
                continue;
            }

            self.log_tool_call_turn(
                turn,
                &offered,
                sent_messages,
                &response,
                &usage_delta,
                &turn_span,
            );

            // --- pre-pass: special-case tools (in-process, never reach ToolRuntime) ---
            let mut doom_hit: Option<String> = None;
            let mut cycle_hit: Option<Vec<String>> = None;
            let submitted_report = self
                .run_prepass(
                    turn,
                    &response,
                    messages,
                    wrapping_up,
                    scratchpad,
                    &mut malformed_reports,
                )
                .await?;

            // --- partition remaining regular tools into read/write ---
            let regular: Vec<_> = response
                .tool_calls
                .iter()
                .filter(|c| {
                    c.name != SUBMIT_REPORT_TOOL && c.name != SCRATCHPAD_TOOL && !wrapping_up
                })
                .collect();

            let (reads, writes): (Vec<_>, Vec<_>) =
                regular.iter().partition(|c| runtime.is_read_only(&c.name));

            // Run read-only tools concurrently.
            //
            // `join_all` preserves input order, so results are zipped back onto `reads` by
            // position. Matching on `call.id` instead would be wrong: nothing guarantees a model
            // gives two calls in one batch distinct ids, and a repeated id made `find` return the
            // same call for both — attributing one tool's output to another and answering that id
            // twice. Position is the only correlation the batch actually has.
            if !reads.is_empty() {
                self.run_reads(
                    turn,
                    &reads,
                    runtime,
                    messages,
                    &mut call_history,
                    &mut repeat_calls,
                    &mut doom_hit,
                    &mut cycle_hit,
                    &policy,
                )
                .await;
            }

            // Run write tools serially.
            self.run_writes(
                turn,
                &writes,
                runtime,
                messages,
                &mut call_history,
                &mut repeat_calls,
                &mut doom_hit,
                &mut cycle_hit,
                &policy,
            )
            .await;

            if let Some(report) = submitted_report {
                // Stamped here so the count covers the whole batch, including calls processed
                // after `submit_report` was parsed (see the decode arm above).
                return Ok(Terminal::Filed(report.with_repeat_calls(repeat_calls)));
            }

            // Escalations only after every tool_call_id has a result message.
            if let Some(tool_name) = &doom_hit {
                escalate_doom(
                    turn,
                    &mut doom_guard,
                    tools,
                    messages,
                    &mut bonus_granted,
                    &mut max_turns,
                    tool_name,
                );
                continue 'turn_loop;
            }
            if let Some(cycling) = &cycle_hit {
                escalate_cycle(
                    turn,
                    &mut cycle_guard,
                    tools,
                    messages,
                    &mut bonus_granted,
                    &mut max_turns,
                    cycling,
                );
                continue 'turn_loop;
            }
        };

        budget_exhausted_outcome(exhausted_name, max_turns, mode, &call_history, repeat_calls)
    }

    /// Record a tool-calling turn in the trace and log it, returning the tool count. The two
    /// `tracing::info!` calls live here (each costs ~8 in clippy's cognitive-complexity model, so
    /// keeping them out of the hot loop body keeps the loop itself measurable).
    #[allow(clippy::too_many_arguments)]
    fn log_tool_call_turn(
        &self,
        turn: u32,
        offered: &[String],
        sent_messages: usize,
        response: &CompletionResponse,
        usage_delta: &(u32, u32),
        turn_span: &tracing::Span,
    ) -> usize {
        let tool_count = response.tool_calls.len();
        turn_span.record("tool_calls", tool_count);
        turn_span.record("finish_reason", "tool_calls");
        let called: Vec<String> = response.tool_calls.iter().map(|c| c.name.clone()).collect();
        self.observe_turn(
            turn,
            offered,
            sent_messages,
            response.content.as_deref(),
            "tool_calls",
            &called,
            usage_delta,
        );
        if tool_count > 0 {
            let names: Vec<&str> = response
                .tool_calls
                .iter()
                .map(|c| c.name.as_str())
                .collect();
            tracing::info!(turn, tool_count, ?names, "turn called tools");
            log_reasoning_if_any(turn, response);
        }
        tool_count
    }

    /// The budget check for one turn: which bound ran out, if any, and — when the work is
    /// salvageable — the one-time wrap-up reserve grant that lets the model file a partial report
    /// instead of losing everything. All-or-nothing work fails exactly as it always has: a
    /// half-applied change is not partial credit, and reporting it as such would misstate the
    /// world. Returns the exhausted resource name for the caller to break the loop with.
    #[allow(clippy::too_many_arguments)]
    fn exhaustion_step(
        &self,
        turn: u32,
        usage: &ResourceUsage,
        mode: Mode,
        policy: &RunPolicy,
        wrapping_up: &mut bool,
        max_turns: &mut u32,
        tools: &mut Vec<ToolDef>,
        messages: &mut Vec<Message>,
    ) -> Option<&'static str> {
        let exhausted = if turn > *max_turns {
            Some("turns")
        } else if *wrapping_up {
            None
        } else {
            self.budget.exhausted_extra(usage)
        };
        let name = exhausted?;
        conversation_reserve::after_named_exhaustion(
            wrapping_up,
            max_turns,
            tools,
            messages,
            turn,
            name,
            mode,
            policy,
        )
    }

    /// Handle a prose-only completion: conversational modes end with the spoken text; report mode
    /// nudges once toward `submit_report` and otherwise wraps the prose as the filed report.
    /// `Some(terminal)` ends the run; `None` means the loop continues (the nudge case).
    #[allow(clippy::too_many_arguments)]
    fn handle_prose(
        &self,
        turn: u32,
        offered: &[String],
        sent_messages: usize,
        response: &CompletionResponse,
        mode: Mode,
        nudged: &mut bool,
        messages: &mut Vec<Message>,
        usage_delta: &(u32, u32),
        repeat_calls: usize,
        turn_span: &tracing::Span,
    ) -> Option<Terminal> {
        let text = response.content.clone().unwrap_or_default();
        turn_span.record("finish_reason", "prose");
        self.observe_turn(
            turn,
            offered,
            sent_messages,
            Some(&text),
            "prose",
            &[],
            usage_delta,
        );
        match mode {
            Mode::Conversational => Some(Terminal::Spoke(text)),
            Mode::Report if !*nudged => {
                *nudged = true;
                tracing::debug!(
                    turn,
                    "model replied with prose; nudging to use submit_report"
                );
                messages.push(Message::user(REPORT_NUDGE));
                None
            }
            Mode::Report => {
                tracing::warn!(
                    turn,
                    "executor finished without submit_report; wrapping prose as Report"
                );
                Some(Terminal::Filed(
                    prose_report(text).with_repeat_calls(repeat_calls),
                ))
            }
        }
    }

    /// Pre-pass over the turn's tool calls: the in-process tools (`submit_report`, scratchpad)
    /// and the wrap-up refusal never reach the `ToolRuntime`. The arms are mutually exclusive on
    /// purpose: OpenAI-compat providers require exactly one tool-result message per
    /// `tool_call_id` (dogfood D3, 01KX7AGD), so a call that matches two categories — a
    /// scratchpad update arriving while the wrap-up reserve is running — must be answered once,
    /// by the first arm that claims it. Precedence is the same order the single-pass loop used
    /// before the read/write split: finish, then the wrap-up refusal, then the scratchpad.
    #[allow(clippy::too_many_arguments)]
    async fn run_prepass(
        &self,
        turn: u32,
        response: &CompletionResponse,
        messages: &mut Vec<Message>,
        wrapping_up: bool,
        scratchpad: &mut Option<Scratchpad>,
        malformed_reports: &mut u32,
    ) -> Result<Option<Report>, ExecError> {
        let mut submitted_report: Option<Report> = None;
        for call in &response.tool_calls {
            if call.name == SUBMIT_REPORT_TOOL {
                if let Some(report) = self
                    .handle_submit_report(turn, call, wrapping_up, malformed_reports, messages)
                    .await?
                {
                    submitted_report = Some(report);
                }
            } else if wrapping_up {
                // Withdrawing a tool from the offered catalog only changes what the model is
                // *shown*; nothing stops it calling a name it still remembers from earlier
                // turns. During the reserve that distinction matters — the whole point is that
                // the extra turns cannot buy more work — so refuse outright and say why.
                tracing::debug!(turn, tool = %call.name, "refused a tool call during wrap-up");
                let shown = format!(
                    "`{}` is no longer available — you are out of budget. Call `{}` with \
                     what you have.",
                    call.name, SUBMIT_REPORT_TOOL
                );
                self.mvl_tool_started(turn, call);
                self.mvl_tool_result(turn, call, false, &shown);
                messages.push(Message::tool_result(&call.id, shown));
            } else if let Some(pad) = scratchpad
                && call.name == SCRATCHPAD_TOOL
            {
                // Engine-injected, like `submit_report`: handled in-process, never reaches
                // `ToolRuntime`, and — deliberately — never enters doom-loop/cycle tracking.
                // Legitimate scratchpad usage would otherwise misfire both guards.
                let result = pad.apply(&call.arguments);
                self.mvl_tool_started(turn, call);
                self.mvl_tool_result(turn, call, true, &result);
                messages.push(Message::tool_result(&call.id, result));
            }
        }
        Ok(submitted_report)
    }

    /// Run the read-only tools of one turn concurrently. `join_all` preserves input order, so
    /// results are zipped back onto `reads` by position. Matching on `call.id` instead would be
    /// wrong: nothing guarantees a model gives two calls in one batch distinct ids, and a
    /// repeated id made `find` return the same call for both — attributing one tool's output to
    /// another and answering that id twice. Position is the only correlation the batch actually
    /// has.
    #[allow(clippy::too_many_arguments)]
    async fn run_reads(
        &self,
        turn: u32,
        reads: &[&ToolInvocation],
        runtime: &dyn ToolRuntime,
        messages: &mut Vec<Message>,
        call_history: &mut Vec<(String, serde_json::Value, String)>,
        repeat_calls: &mut usize,
        doom_hit: &mut Option<String>,
        cycle_hit: &mut Option<Vec<String>>,
        policy: &RunPolicy,
    ) {
        let futures: Vec<_> = reads
            .iter()
            .map(|call: &&ToolInvocation| {
                let call = (*call).clone();
                async move {
                    let tool_span =
                        tracing::debug_span!("tool_call", name = %call.name, id = %call.id);
                    async {
                        run_tool(
                            runtime,
                            &call,
                            self.spill_dir.as_deref(),
                            self.spill_max_bytes,
                        )
                        .await
                    }
                    .instrument(tool_span)
                    .await
                }
            })
            .collect();
        let read_results = futures::future::join_all(futures).await;
        for (call, result) in reads.iter().zip(read_results) {
            self.mvl_tool_started(turn, call);
            let ok = !result.starts_with("tool error:");
            self.mvl_tool_result(turn, call, ok, &result);
            call_history.push((call.name.clone(), call.arguments.clone(), result.clone()));
            if call_history[..call_history.len() - 1]
                .iter()
                .any(|(n, a, _)| n == &call.name && a == &call.arguments)
            {
                *repeat_calls += 1;
            }
            messages.push(Message::tool_result(&call.id, result));
        }
        if doom_hit.is_none() && is_doom_loop(call_history, policy.loop_profile) {
            *doom_hit = Some("(read batch)".to_string());
        }
        if cycle_hit.is_none()
            && let Some(cycling) = detect_short_cycle(call_history)
        {
            *cycle_hit = Some(cycling);
        }
    }

    /// Run the write tools of one turn serially.
    #[allow(clippy::too_many_arguments)]
    async fn run_writes(
        &self,
        turn: u32,
        writes: &[&ToolInvocation],
        runtime: &dyn ToolRuntime,
        messages: &mut Vec<Message>,
        call_history: &mut Vec<(String, serde_json::Value, String)>,
        repeat_calls: &mut usize,
        doom_hit: &mut Option<String>,
        cycle_hit: &mut Option<Vec<String>>,
        policy: &RunPolicy,
    ) {
        for call in writes {
            let tool_span = tracing::debug_span!("tool_call", name = %call.name, id = %call.id);
            self.mvl_tool_started(turn, call);
            let result = async {
                run_tool(
                    runtime,
                    call,
                    self.spill_dir.as_deref(),
                    self.spill_max_bytes,
                )
                .await
            }
            .instrument(tool_span)
            .await;
            let ok = !result.starts_with("tool error:");
            self.mvl_tool_result(turn, call, ok, &result);
            call_history.push((call.name.clone(), call.arguments.clone(), result.clone()));
            if call_history[..call_history.len() - 1]
                .iter()
                .any(|(n, a, _)| n == &call.name && a == &call.arguments)
            {
                *repeat_calls += 1;
            }
            messages.push(Message::tool_result(&call.id, result));
            if doom_hit.is_none() && is_doom_loop(call_history, policy.loop_profile) {
                *doom_hit = Some(call.name.clone());
            }
            if cycle_hit.is_none()
                && let Some(cycling) = detect_short_cycle(call_history)
            {
                *cycle_hit = Some(cycling);
            }
        }
    }

    /// A gate refusal of a live `succeeded`: feed the check text back in-band and decide whether
    /// the host itself failed (infrastructure — do not ask the model) or the model must fix the
    /// work. Returns the report to end the run with, if any.
    #[allow(clippy::too_many_arguments)]
    fn handle_refused_report(
        &self,
        turn: u32,
        call: &ToolInvocation,
        report: Report,
        shown: String,
        messages: &mut Vec<Message>,
    ) -> Option<Report> {
        self.mvl_tool_started(turn, call);
        self.mvl_tool_result(turn, call, false, &shown);
        messages.push(Message::tool_result(&call.id, shown.clone()));
        if shown
            .to_ascii_lowercase()
            .contains("failure_class: infrastructure")
        {
            tracing::warn!(
                turn,
                "submit_report succeeded refused: host failed; not asking the model"
            );
            Some(Report {
                outcome: Outcome::Failed,
                summary: shown,
                ..report
            })
        } else {
            tracing::info!(
                turn,
                outcome = ?report.outcome,
                "submit_report succeeded was not accepted; handing the check back"
            );
            None
        }
    }

    /// The workspace gate for one report, when it applies: `None` means accepted (or no gate is
    /// configured for this outcome); `Some(shown)` is the refusal text handed back to the model.
    /// Only a live `succeeded` may be a lie, and only then do we ask the gate — a refusal is a
    /// tool result, not a reset.
    async fn accept_report(&self, report: &Report, wrapping_up: bool) -> Option<String> {
        if Self::report_ends_without_gate(report.outcome, wrapping_up) {
            return None;
        }
        let Some(gate) = &self.report_gate else {
            return None;
        };
        gate.accept(report, wrapping_up).await.err()
    }

    /// One `submit_report` call: parse it against the Report schema, run the workspace gate for a
    /// live `succeeded` (a refusal is a tool result, not a reset), and hand malformed argument
    /// objects back to the model for correction — bounded, since a model that cannot produce the
    /// shape will not discover it by repetition. Returns the report to end the run with, if any.
    #[allow(clippy::too_many_arguments)]
    async fn handle_submit_report(
        &self,
        turn: u32,
        call: &ToolInvocation,
        wrapping_up: bool,
        malformed_reports: &mut u32,
        messages: &mut Vec<Message>,
    ) -> Result<Option<Report>, ExecError> {
        match serde_json::from_value::<Report>(call.arguments.clone()) {
            Ok(report) => {
                // Partial / Failed / wrap-up end the loop as-is. The worktree is not
                // reverted: half-finished files stay for the next attempt or a human.
                // Only a live `succeeded` may be a lie, and only then do we ask the
                // gate — a refusal is a tool result, not a reset.
                if let Some(shown) = self.accept_report(&report, wrapping_up).await {
                    Ok(self.handle_refused_report(turn, call, report, shown, messages))
                } else {
                    tracing::info!(turn, "subagent filed report");
                    self.mvl_tool_started(turn, call);
                    self.mvl_tool_result(turn, call, true, "report accepted");
                    messages.push(Message::tool_result(
                        &call.id,
                        "report accepted".to_string(),
                    ));
                    Ok(Some(report))
                }
            }
            // A malformed argument object is the model getting a schema slightly wrong,
            // which is exactly the class of mistake it can fix when told. Every *other*
            // tool failure is already fed back in-band; this one used to abort the whole
            // run, discarding completed work over a missing field (live: a coding run
            // ended at turn 12 on `missing field \`outcome\``). Hand it the error and let
            // it retry — but bound the retries, since a model that cannot produce the
            // shape will not discover it by repetition.
            Err(e) if *malformed_reports < MAX_MALFORMED_REPORTS => {
                *malformed_reports += 1;
                tracing::warn!(
                    turn,
                    attempt = *malformed_reports,
                    error = %e,
                    "submit_report arguments did not match the Report schema; asking the model to correct them"
                );
                let shown = malformed_report_nudge(&e);
                self.mvl_tool_started(turn, call);
                self.mvl_tool_result(turn, call, false, &shown);
                messages.push(Message::tool_result(&call.id, shown));
                Ok(None)
            }
            Err(e) => Err(ExecError::Decode(e.to_string())),
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
            let result = async {
                run_tool(
                    runtime,
                    inv,
                    self.spill_dir.as_deref(),
                    self.spill_max_bytes,
                )
                .await
            }
            .instrument(span)
            .await;
            messages.push(Message::tool_result(&inv.id, result));
        }
    }
}

/// Run one tool call, folding a tool-level error into an in-band result string so the model can
/// adapt rather than the loop aborting.
/// When `spill_dir` is set and the result exceeds `spill_max_bytes`, write the full
/// body and return a head+tail preview. No directory: pass through unchanged.
fn run_tool_spill(
    result: &str,
    spill_dir: Option<&std::path::Path>,
    spill_max_bytes: usize,
    label: &str,
) -> String {
    match spill_dir {
        Some(dir) => spill_oversized_result(result, spill_max_bytes, dir, label).0,
        None => result.to_string(),
    }
}

async fn run_tool(
    runtime: &dyn ToolRuntime,
    call: &ToolInvocation,
    spill_dir: Option<&std::path::Path>,
    spill_max_bytes: usize,
) -> String {
    let raw = match runtime.invoke(call).await {
        Ok(content) => content,
        Err(message) => format!("tool error: {message}"),
    };
    run_tool_spill(&raw, spill_dir, spill_max_bytes, &call.id)
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
        repeat_calls: 0,
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
        repeat_calls: 0,
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
        repeat_calls: 0,
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
        repeat_calls: 0,
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

/// Property tests over the doom-loop guard's similarity primitives — [`args_similarity`],
/// [`cosine`], and [`tokenize`]. All three are small, pure, and deterministic, so proptest can
/// fuzz them with arbitrary JSON argument trees: the caller of `run_loop` feeds real model output
/// into these, so *any* shape must be safe to score, not just the hand-written calibration cases.
#[cfg(test)]
mod proptest_tests {
    use proptest::prelude::*;
    use serde_json::Value;

    use crate::loop_guard::{args_similarity, tokenize};

    /// Arbitrary JSON tool-argument trees, 0-4 levels deep, with numbers/strings/bools/arrays/
    /// objects. Strings run 0-200 chars. `any::<f64>()` also draws NaN/±inf/-0.0/subnormals;
    /// serde_json cannot represent a non-finite number, so `Value::from` collapses those to
    /// `Null` (total, never panics) — the bit patterns still flow through every function under
    /// test, which is the point: none of them may blow up on any input.
    fn arb_json_value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::from),
            any::<f64>().prop_map(Value::from),
            proptest::collection::vec(proptest::char::range('\u{20}', '\u{7e}'), 0..=200)
                .prop_map(|chars| Value::String(chars.into_iter().collect())),
        ];
        leaf.prop_recursive(4, 16, 8, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
                proptest::collection::hash_map("[a-zA-Z0-9_-]{0,12}", inner, 0..8).prop_map(|m| {
                    let mut map = serde_json::Map::new();
                    for (k, v) in m {
                        map.insert(k, v);
                    }
                    Value::Object(map)
                }),
            ]
        })
    }

    /// `args_similarity` must not depend on which side is which — the doom-loop guard compares
    /// calls in history order, so the score has to be order-agnostic. Both runs go through the
    /// same TF-IDF/IDF computation, so the only way they differ is f32 summation order; 1e-5 is
    /// comfortably above that noise.
    fn similarity_symmetric(a: Value, b: Value) -> bool {
        let s1 = args_similarity(&a, &b);
        let s2 = args_similarity(&b, &a);
        (s1 - s2).abs() < 1e-5
    }

    /// A value is always identical to itself — `args_similarity(x, x)` is the top of the scale.
    fn similarity_reflexive(x: Value) -> bool {
        (args_similarity(&x, &x) - 1.0).abs() < 1e-5
    }

    /// The output is a similarity: never negative, never above 1. Cosine clamps f32 noise
    /// that would otherwise round a hair past 1.0 on near-duplicate bags.
    fn similarity_in_range(a: Value, b: Value) -> bool {
        let s = args_similarity(&a, &b);
        (0.0..=1.0).contains(&s)
    }

    /// `tokenize` is total: no JSON shape may make it panic.
    fn tokenize_never_panics(v: Value) -> bool {
        let _ = tokenize(&v);
        true
    }

    proptest! {
        #[test]
        fn proptest_args_similarity_is_symmetric(a in arb_json_value(), b in arb_json_value()) {
            prop_assert!(similarity_symmetric(a, b));
        }

        #[test]
        fn proptest_args_similarity_is_reflexive(x in arb_json_value()) {
            prop_assert!(similarity_reflexive(x));
        }

        #[test]
        fn proptest_args_similarity_stays_in_unit_range(a in arb_json_value(), b in arb_json_value()) {
            prop_assert!(similarity_in_range(a, b));
        }

        #[test]
        fn proptest_tokenize_never_panics(v in arb_json_value()) {
            prop_assert!(tokenize_never_panics(v));
        }
    }
}

#[cfg(test)]
#[path = "lib_survivor_tests.rs"]
mod survivor_tests;
