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
mod mvl;
mod risk_gated;

pub use budget::{Budget, ResourceLimit, ResourceUsage, TokenLimit, WallClockLimit};
pub use mvl::MvlSession;
pub use risk_gated::RiskGatedToolRuntime;

use std::path::PathBuf;
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

/// How many consecutive, *near-duplicate* invocations of the same tool count as a "doom loop" — the
/// model succeeding at a tool call every time yet making no progress, rather than hitting an error
/// it could react to. Matches the threshold comparable harnesses use for the same failure mode
/// (opencode/kilocode's `DOOM_LOOP_THRESHOLD`, VTCode's `LoopDetector`) — evidence this needs an
/// engine-level guard, not just prompt wording, came from a live reproduction of
/// `docs/future-work/archive/multi-step-execution-reliability-finding.md`: DeepSeek and Gemini both got stuck
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

/// How strictly two consecutive same-tool calls must resemble each other to count as a repeat.
///
/// The semantic bar is right for acting work, where re-issuing a nearly-identical call is almost
/// always thrash. It is wrong for **search**: "orchestration anti-patterns" and "agentic AI
/// failure modes" are different queries that a bag-of-words comparison scores as near-duplicates,
/// and a live deep-research run was stopped three times for exactly that — legitimate query
/// variation read as a loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArgMatch {
    /// Near-duplicate arguments count as a repeat ([`ARG_SIMILARITY_THRESHOLD`]).
    #[default]
    Semantic,
    /// Only byte-identical arguments count. Re-running the *same* query still trips the guard —
    /// that is real thrash — but varied queries never do.
    Exact,
}

/// The per-run behaviour `run_loop` needs from its [`Task`]: how repeats are judged, and whether
/// partial work is worth filing. Grouped rather than passed as loose arguments — they travel
/// together and always come from the same place.
#[derive(Debug, Clone, Copy, Default)]
struct RunPolicy {
    salvageable: bool,
    loop_profile: LoopProfile,
}

/// Per-task loop-detection settings. Separate from [`Budget`] because it tunes what counts as a
/// problem, not how much of the resource is left.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopProfile {
    pub arg_match: ArgMatch,
}

impl LoopProfile {
    /// The default: near-duplicate arguments count as a repeat.
    pub fn semantic() -> Self {
        Self {
            arg_match: ArgMatch::Semantic,
        }
    }

    /// For search-shaped work, where varied queries are the job rather than a symptom.
    pub fn exact() -> Self {
        Self {
            arg_match: ArgMatch::Exact,
        }
    }
}

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

    /// Whether a tool is safe to run concurrently with other tool calls in the same turn.
    /// Read-only tools (file reads, searches, git inspection) return true.
    /// Default: false (conservative — treat every tool as potentially stateful).
    fn is_read_only(&self, _tool_name: &str) -> bool {
        false
    }

    /// After this tool returns, stop the conversational loop and wait for the
    /// human's next message. The tool result is *not* written until that answer
    /// arrives (ACP cannot overlap two `session/prompt`s).
    fn parks_for_human(&self, _tool_name: &str) -> bool {
        false
    }
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

    /// Like [`runtime_for`](Self::runtime_for), but scoped to a per-worker workspace root.
    ///
    /// `workspace_root` is `Some(path)` when the worker must operate inside an isolated
    /// filesystem workspace (a git worktree, in the coding pack's world) and `None` when the
    /// worker is unconstrained. The **default** implementation ignores the root and behaves
    /// exactly like [`runtime_for`](Self::runtime_for) — factories that do not care about
    /// workspace isolation (the MCP registry, test mocks) never need to override this.
    ///
    /// Placement (backlog C7): the *seam* is kernel-side — an orchestrator that fans work out
    /// passes the root through untouched — but the *isolation* is a pack concern. The concrete
    /// worktree primitive lives in `coder-sandbox` (pack); the production caller builds the
    /// workspaces and supplies a factory that roots each worker's runtime in one. The kernel
    /// never reaches across the layer line for the primitive itself.
    async fn runtime_for_in(
        &self,
        allowed_mcps: &[String],
        provenance: WriteProvenance,
        workspace_root: Option<PathBuf>,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        let _ = workspace_root;
        self.runtime_for(allowed_mcps, provenance).await
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
        let tools = runtime.catalog();
        self.mvl_start(None);
        let result = async {
            for turn in 1..=self.budget.max_turns {
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
                if response.tool_calls.is_empty() {
                    self.mvl_end("succeeded", "model finished");
                    return Ok(()); // the prose answer was streamed as tokens
                }

                for call in &response.tool_calls {
                    if let Some(call_id) = self
                        .invoke_stream_tool(runtime, call, turn, events, messages)
                        .await?
                    {
                        return Err(ExecError::AwaitingHuman { call_id });
                    }
                }
                tracing::info!(turn, tools = response.tool_calls.len(), "turn used tools");
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
        if *wrapping_up || !policy.salvageable || !matches!(mode, Mode::Report) {
            return Some(name);
        }
        *wrapping_up = true;
        *max_turns = turn + WRAP_UP_TURNS - 1;
        // Withdraw everything but the finish tool, so the reserve cannot be spent
        // continuing the work it was granted to conclude.
        tools.retain(|t| t.name == SUBMIT_REPORT_TOOL);
        messages.push(Message::user(wrap_up_directive(name, WRAP_UP_TURNS)));
        tracing::warn!(
            turn,
            resource = name,
            reserve = WRAP_UP_TURNS,
            "budget exhausted on salvageable work; granting wrap-up reserve to file a \
             partial report"
        );
        None
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

/// Whether the last [`DOOM_LOOP_THRESHOLD`] invocations are consecutively the same tool, called
/// with near-duplicate arguments (see `args_similarity`) — see [`DOOM_LOOP_THRESHOLD`]'s doc
/// comment for why near-duplicate, not just byte-identical, is the right bar.
/// Tools whose arguments *are* file content, and which therefore cannot be judged by similarity.
///
/// Two different edits to the same file are always textually alike: same `path`, same language,
/// overlapping identifiers, often overlapping lines. [`args_similarity`] scores that pair high and
/// is not wrong to — it is measuring "same file", which for a search tool is a good proxy for "same
/// action" and for an edit tool is no proxy at all. Editing one file repeatedly is what applying a
/// change looks like.
///
/// Measured, not assumed. In an A/B on 2026-08-11 the coding pack made four consecutive `edit_file`
/// calls — one moving a test helper, one fixing an assertion, two adding tests, across two files —
/// and the guard withdrew `edit_file` on the next turn. It lost `apply_patch` and `run_command`
/// later the same way, and the run ended with the model saying it knew which two call sites were
/// broken and had no tool left to fix them. Kilo Code made 36 edits on the same task, was never
/// disarmed, and shipped a clean pass.
///
/// For these tools only an identical call counts as a repeat. That still catches the real
/// pathology — replaying the byte-identical edit achieves nothing however many times you send it —
/// while a different edit is progress by definition.
fn arguments_are_file_content(tool: &str) -> bool {
    matches!(
        tool,
        "edit_file"
            | "write_file"
            | "apply_patch"
            | "edit"
            | "write"
            | "patch"
            | "multiedit"
            // `run_command` is many programs under one name. Semantic similarity
            // on `rg` + a shared path withdrew it on compare 7 after three
            // different searches. Identical replay is still a doom loop.
            | "run_command"
            | "run_command_background"
            | "bash"
            | "exec"
    )
}

/// Whether two calls of `tool` count as the same action.
///
/// Inspect tools follow `profile`: same path is the same look. File-content
/// tools ([`arguments_are_file_content`]) require byte-identical arguments —
/// two edits of the same file with different `old`/`new` are progress;
/// replaying the same edit is not.
fn arguments_repeat(
    tool: &str,
    a: &serde_json::Value,
    b: &serde_json::Value,
    profile: LoopProfile,
) -> bool {
    let kind = if arguments_are_file_content(tool) {
        ArgMatch::Exact
    } else {
        profile.arg_match
    };
    match kind {
        ArgMatch::Exact => a == b,
        ArgMatch::Semantic => args_similarity(a, b) >= ARG_SIMILARITY_THRESHOLD,
    }
}

fn is_doom_loop(history: &[(String, serde_json::Value, String)], profile: LoopProfile) -> bool {
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
        .all(|pair| arguments_repeat(last_name, pair[0], pair[1], profile))
}

/// Whether the tail of `history` is a short repeating cycle (period 2 or 3 — e.g. A,B,A,B or
/// A,B,C,A,B,C) *over the same arguments*. Returns the distinct tool names participating in the
/// cycle (so the caller can remove exactly those, not the whole catalog) rather than a bare bool.
///
/// Matching tool names is necessary but **not** sufficient. `read_file(a)`, `search_text(x)`,
/// `read_file(b)`, `search_text(y)` is what reading an unfamiliar codebase looks like: the names
/// alternate, but every call names a different resource and every call makes progress. Requiring
/// the positionally-corresponding arguments to repeat — inspect slots via [`args_similarity`]
/// and [`IDENTITY_ARG_KEYS`], file-content slots via exact args (see
/// [`arguments_repeat`]) — separates that from genuine thrash. Same-file
/// `read`/`edit` with a different `old`/`new` is the mandated coding loop, not
/// a cycle; replaying the same edit is.
///
/// This was not academic: with names-only matching the guard fired on turn 4 of a 60-turn coding
/// run, removed `read_file`/`search_text` for the rest of the task, and the model filed a complete
/// implementation plan it had no remaining way to carry out ("blocked from making edits by the
/// progress guard"). Period 2 needs only four calls, so *any* task requiring more than four
/// alternating inspections was unreachable.
///
/// A mono-tool streak (`read_note`×4 in one parallel batch) is **not** a cycle — period-2 would
/// match `AAAA` as two copies of `AA`, which is a false positive that used to mid-batch-nudge and
/// leave unanswered `tool_call_id`s (dogfood session `01KX7BWV`). Same-tool thrash is
/// [`is_doom_loop`]'s job.
fn detect_short_cycle(history: &[(String, serde_json::Value, String)]) -> Option<Vec<String>> {
    for period in 2..=3 {
        let window = period * 2;
        if history.len() < window {
            continue;
        }
        let tail = &history[history.len() - window..];
        let (first_half, second_half) = tail.split_at(period);
        // Same tool in the same slot of both halves...
        if !first_half
            .iter()
            .zip(second_half)
            .all(|((a_name, ..), (b_name, ..))| a_name == b_name)
        {
            continue;
        }
        // ...called on the same thing. Inspect slots use path identity;
        // file-content slots (edit/write/`run_command`) require identical
        // arguments. Same-file read → edit → read → edit is the mandated
        // loop when the edits differ; replaying the same edit is a cycle.
        if !first_half
            .iter()
            .zip(second_half)
            .all(|((a_name, a_args, _), (_, b_args, _))| {
                arguments_repeat(a_name, a_args, b_args, LoopProfile::semantic())
            })
        {
            continue;
        }
        let mut distinct: Vec<String> = first_half.iter().map(|(name, ..)| name.clone()).collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));

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
        let runtime = MockToolRuntime::new(&["apply_patch"], Ok("applied".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

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

    const REAL_EDIT_1: &str = r#"{"path":"crates/acp-bridge/src/main.rs","old":"        assert_eq!(result[\\\"protocolVersion\\\"], PROTOCOL_VERSION);\n        assert_eq!(result[\\\"agentInfo\\\"][\\\"name\\\"], \\\"Liberado\\\");\n        // Must stay false until durable load+replay (P3); true lied to Paseo's resume path.\n        assert_eq!(result[\\\"agentCapabilities\\\"][\\\"loadSession\\\"], false);","new":"        assert_eq!(result[\\\"protocolVersion\\\"], PROTOCOL_VERSION);\n        assert_eq!(result[\\\"agentInfo\\\"][\\\"name\\\"], \\\"Liberado\\\");\n        assert_eq!(\n            result[\\\"agentCapabilities\\\"][\\\"loadSession\\\"],\n            LOAD_SESSION_CAPABILITY,\n            \\\"initialize must reflect LOAD_SESSION_CAPABILITY exactly\\\"\n        );"}"#;
    const REAL_EDIT_2: &str = r#"{"path":"crates/acp-bridge/src/main.rs","new":"    #[tokio::test]\n    async fn load_then_prompt_appends_to_existing_transcript() {\n        let dir = tempfile::TempDir::new().unwrap();\n        let _guards = with_session_dir(&dir);\n\n        let sid = \"lib-append-after-load\";\n        session_store::save(&session_store::SessionRecord {\n            id: sid.to_string(),\n            mode: \"coding\".to_string(),\n            cwd: std::path::PathBuf::from(\"/tmp/proj\"),\n            model: \"m1\".to_string(),\n            messages: vec![\n                session_store::StoredMessage {\n                    role: \"user\".into(),\n                    content: \"initial question\".into(),\n                },\n                session_store::StoredMessage {\n                    role: \"assistant\".into(),\n                    content: \"initial answer\".into(),\n                },\n            ],\n            updated_at: \"2025-01-01T00:00:00Z\".into(),\n        })\n        .expect(\"save\");\n\n        let bridge = test_bridge();\n        let sink = CaptureSink {\n            lines: std::sync::Mutex::new(Vec::new()),\n        };\n        let _result = handle_request(\n            bridge.clone(),\n            &sink,\n            \"session/load\",\n            json!({ \"sessionId\": sid }),\n        )\n        .await\n        .expect(\"load must succeed\");\n\n        // Verify the session is registered in-memory with the right mode/cwd.\n        {\n            let sessions = bridge.acp_sessions.lock().await;\n            let sess = sessions\n                .get(sid)\n                .expect(\"session must be registered after load\");\n            assert_eq!(sess.mode, AgentMode::Coding);\n            assert_eq!(sess.cwd, std::path::PathBuf::from(\"/tmp/proj\"));\n        }\n\n        // Simulate what run_session_prompt does after a turn: persist new messages.\n        session_store::append_messages(sid, \"new question\", \"new answer\")\n            .expect(\"append must succeed\");\n\n        let loaded = session_store::load(sid)\n            .expect(\"load\")\n            .expect(\"record must be present\");\n\n        assert_eq!(loaded.messages.len(), 4);\n        assert_eq!(loaded.messages[0].content, \"initial question\");\n        assert_eq!(loaded.messages[1].content, \"initial answer\");\n        assert_eq!(loaded.messages[2].content, \"new question\");\n        assert_eq!(loaded.messages[3].content, \"new answer\");\n    }","old":"    #[tokio::test]\n    async fn load_then_prompt_appends_to_existing_transcript() {\n        let dir = tempfile::TempDir::new().unwrap();\n        let _guards = with_session_dir(&dir);\n\n        let sid = \"lib-append-after-load\";\n        session_store::save(&session_store::SessionRecord {\n            id: sid.to_string(),\n            mode: \"coding\".to_string(),\n            cwd: std::path::PathBuf::from(\"/tmp/proj\"),\n            model: \"m1\".to_string(),\n            messages: vec![\n                session_store::StoredMessage {\n                    role: \"user\".into(),\n                    content: \"initial question\".into(),\n                },\n                session_store::StoredMessage {\n                    role: \"assistant\".into(),\n                    content: \"initial answer\".into(),\n                },\n            ],\n            updated_at: \"2025-01-01T00:00:00Z\".into(),\n        })\n        .expect(\"save\");\n\n        let bridge = test_bridge();\n        let sink = CaptureSink {\n            lines: std::sync::Mutex::new(Vec::new()),\n        };\n        let _result = handle_request(\n            bridge,\n            &sink,\n            \"session/load\",\n            json!({ \"sessionId\": sid }),\n        )\n        .await\n        .expect(\"load must succeed\");\n\n        // Now append messages (simulating what run_session_prompt does after a prompt).\n        session_store::append_messages(sid, \"new question\", \"new answer\")\n            .expect(\"append must succeed\");\n\n        let loaded = session_store::load(sid)\n            .expect(\"load\")\n            .expect(\"record must be present\");\n\n        assert_eq!(loaded.messages.len(), 4);\n        assert_eq!(loaded.messages[0].content, \"initial question\");\n        assert_eq!(loaded.messages[1].content, \"initial answer\");\n        assert_eq!(loaded.messages[2].content, \"new question\");\n        assert_eq!(loaded.messages[3].content, \"new answer\");\n    }"}"#;
    const REAL_EDIT_3: &str = r#"{"new":"    }\n}","old":"    }\n\n    #[test]\n    fn initialize_advertises_load_session_capability() {\n        // Must be true once load is implemented.\n        assert!(\n            LOAD_SESSION_CAPABILITY,\n            \"initialize must advertise loadSession:true now that session/load restores history\"\n        );\n    }\n}","path":"crates/acp-bridge/src/main.rs"}"#;

    /// The three consecutive `edit_file` calls that actually got the tool withdrawn, verbatim from
    /// `coder-traces/lib-18ca9815159fee44-22288-attempt-2`. Recorded arguments, not a reconstruction:
    /// a synthetic pair I wrote by hand did *not* reproduce the failure, which is how I learned the
    /// hand-written version was testing nothing.
    #[test]
    fn the_real_recorded_edits_are_not_a_doom_loop() {
        let hist: Vec<(String, serde_json::Value, String)> =
            [REAL_EDIT_1, REAL_EDIT_2, REAL_EDIT_3]
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
        let runtime = MockToolRuntime::new(&["edit_file", "read_file"], Ok("done".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("same result".into()));

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
        let runtime = MockToolRuntime::new(&["tool-a", "tool-b"], Ok("result".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Err("boom".into()));
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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));

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

    #[test]
    fn cosine_of_identical_vectors_is_one() {
        let mut v = std::collections::HashMap::new();
        v.insert("hello".into(), 1.0);
        v.insert("world".into(), 2.0);
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_orthogonal_vectors_is_zero() {
        let mut a = std::collections::HashMap::new();
        a.insert("hello".into(), 1.0);
        let mut b = std::collections::HashMap::new();
        b.insert("world".into(), 1.0);
        assert!((cosine(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_zero_vector_is_zero() {
        let a = std::collections::HashMap::new();
        let mut b = std::collections::HashMap::new();
        b.insert("hello".into(), 1.0);
        assert!((cosine(&a, &b) - 0.0).abs() < 1e-6);
        assert!((cosine(&b, &a) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn args_similarity_default_near_duplicates() {
        let a = serde_json::json!({"q": "hello world"});
        let b = serde_json::json!({"q": "hello world"});
        let sim = args_similarity(&a, &b);
        assert!(
            sim > 0.9,
            "identical plain text should be near-duplicate: {sim}"
        );
    }

    #[test]
    fn args_similarity_neutral_args_still_use_cosine() {
        // Two non-empty, unequal argument sets with no overlap in tokens. The `&&` on line 1247
        // correctly skips the early return to compute TF-IDF; with `||` it would return 0.0.
        // Cosine is also 0.0 for orthogonal vectors, so we verify the function doesn't panic
        // and returns a sub-1 value.
        let a = serde_json::json!({"x": "hello"});
        let b = serde_json::json!({"y": "world"});
        let sim = args_similarity(&a, &b);
        assert!(
            sim < 1.0,
            "different args must not be perfectly similar: {sim}"
        );
    }

    #[test]
    fn args_similarity_empty_one_is_not_one() {
        let a = serde_json::json!({});
        let b = serde_json::json!({"q": "hello"});
        // When one is empty but the other has tokens, similarity must still be computed.
        let sim = args_similarity(&a, &b);
        assert!(
            sim < 1.0,
            "one empty should not produce perfect similarity: {sim}"
        );
    }

    #[test]
    fn tf_idf_smoke() {
        let docs = vec![
            vec!["a".to_string(), "b".to_string()],
            vec!["b".to_string(), "c".to_string()],
        ];
        let vectors = tf_idf_vectors(&docs);
        assert_eq!(vectors.len(), 2);
        // Each vector has 2 entries.
        assert_eq!(vectors[0].len(), 2);
        assert_eq!(vectors[1].len(), 2);
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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));
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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));
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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));
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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));
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
        let runtime = MockToolRuntime::new(&["search"], Ok("hits".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("3 hits".into()));

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
        inner: MockToolRuntime,
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
                    ToolInvocation::new(
                        "read_a",
                        "read_file",
                        serde_json::json!({"path": "a.txt"}),
                    ),
                    ToolInvocation::new("read_b", "search_text", serde_json::json!({"query": "x"})),
                ]),
                submit(valid_report_args()),
            ],
            Budget::default(),
        );
        let runtime = ReadOnlyAwareRuntime {
            inner: MockToolRuntime::new(&["read_file", "search_text"], Ok("data".into())),
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
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));

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

    // ── converse_stream budget exhaustion ────────────────────────────

    #[tokio::test]
    async fn converse_stream_errors_on_budget_exhaustion() {
        let (_provider, exec) = executor(
            vec![call_tool("search"), call_tool("search")],
            Budget::new(1),
        );
        let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));
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
        inner: MockToolRuntime,
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
            inner: MockToolRuntime::new(&["ask_human"], Ok("which crate?".into())),
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
            inner: MockToolRuntime::new(&["ask_human"], Ok("which crate?".into())),
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
        let runtime = MockToolRuntime::new(&["search", "write_file"], Ok("data".into()));

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
        let runtime = MockToolRuntime::new(&["search", "write_file"], Ok("data".into()));

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
        let runtime = MockToolRuntime::new(&["search"], Ok("same result".into()));

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
        let spilled =
            std::fs::read_to_string(dir.path().join("tool-spill-tool-call-1.txt")).unwrap();
        assert_eq!(spilled, result);
    }
}

/// Property tests over the doom-loop guard's similarity primitives — [`args_similarity`],
/// [`cosine`], and [`tokenize`]. All three are small, pure, and deterministic, so proptest can
/// fuzz them with arbitrary JSON argument trees: the caller of `run_loop` feeds real model output
/// into these, so *any* shape must be safe to score, not just the hand-written calibration cases.
#[cfg(test)]
mod proptest_tests {
    use proptest::prelude::*;
    use serde_json::Value;

    use super::args_similarity;
    use super::tokenize;

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

    /// The output is a similarity: never negative, never above 1. (A cosine of two non-empty
    /// vectors can round a hair past 1.0 in f32, but the tolerance-free bound here holds for
    /// generated inputs because independent trees essentially never produce exactly-proportional
    /// TF-IDF vectors.)
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
