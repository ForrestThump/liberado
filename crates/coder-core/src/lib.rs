//! # liberado-coder-core
//!
//! Provider-agnostic contracts for Liberado's Rust-native coding backend. This crate intentionally
//! owns no model loop, filesystem mutation, forge API, or sandbox implementation. It is the narrow
//! waist between PR production, a future TUI/CLI coding surface, eval/tuning harnesses, and the
//! Liberado loop backend.
//!
//! Also hosts **verifier** and **criteria-intake** DTOs (`verify`, `intake`) — domain-agnostic shapes
//! first consumed by the coding pack; see `docs/architecture/verifiers.md`.

mod coherence;
mod intake;
mod trace_view;
mod tuning;
mod verify;

pub use trace_view::{
    CallView, Divergence, FailedCall, ForeignTraceFormat, MessagesExport, RunView, SideBySide,
    TerminalSummary, TraceComparison, TurnView, compare_traces, diverge, format_comparison,
    format_divergence, import_foreign_auto, import_foreign_file, import_foreign_messages,
    load_run_view, load_trace, render_transcript, resolve_trace_path, run_view_from_messages,
    run_view_from_trace, write_messages_export,
};
pub use tuning::{CoderTuning, TraceFormat};

pub use coherence::{
    ContractFinding, Severity, contract_conflicts, contradictions, profile_injected_ids,
};
pub use intake::{
    FreezeAuthority, GoalContract, GoalContractDraft, IntakeOutcome, IntakeQuestion,
    expand_verify_profile_into, intake_outcome_schema, profile_verifiers, sanitize_draft,
    validate_draft,
};
mod acceptance;
pub use acceptance::{VERIFY_CMD_ENV, default_verifiers};
pub use verify::{
    Finding, FindingKind, NamedVerdict, PipelinePolicy, PipelineResult, Verdict, VerdictStatus,
    VerifierSpec, resolve_verifier_specs,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use liberado_common::{Outcome, Report};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable name for the first-party Rust coding backend.
///
/// Naming note (2026-07-12): the string predates the goal/loop vocabulary split
/// (`docs/architecture/agentic-loops.md` §Vocabulary — this backend runs *goals*, not *loops*).
/// The value is config-visible (`dispatch.yaml`, task DB, `CODING_BACKEND`), so it is kept
/// unchanged as a legacy identifier rather than breaking deployments over a word.
pub const LIBERADO_LOOP_BACKEND: &str = "liberado-loop";

/// A single coding task, independent of any forge or queue implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoderTask {
    pub id: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default)]
    pub success_criteria: Vec<String>,
}

impl CoderTask {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            context: None,
            success_criteria: Vec::new(),
        }
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }
}

/// The prepared workspace a backend is allowed to mutate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    /// Absolute host path, or an implementation-defined path understood by the sandbox backend.
    pub root: String,
    /// Branch or ref the diff should be measured against.
    pub base_ref: String,
    /// Optional repo slug for traces and policy overlays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

impl WorkspaceRef {
    pub fn new(root: impl Into<String>, base_ref: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            base_ref: base_ref.into(),
            repo: None,
        }
    }
}

/// How code execution is isolated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
#[derive(Default)]
pub enum SandboxSpec {
    /// For tests/dev only. Production coding runs should prefer Docker or a stronger backend.
    #[default]
    HostLocal,
    Docker(DockerSandboxSpec),
    /// Git worktree isolation: the worker runs in a git-worktree copy of the
    /// workspace root so concurrent workers cannot trample each other.
    /// [`SandboxSpec::HostLocal`] is the sandbox inside the worktree.
    Worktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerSandboxSpec {
    pub image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<SandboxVolume>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxVolume {
    pub host: String,
    pub container: String,
    #[serde(default)]
    pub read_only: bool,
}

/// Command execution policy for coding tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPolicy {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    pub timeout_secs: u64,
    pub output_max_bytes: usize,
}

impl Default for CommandPolicy {
    fn default() -> Self {
        Self {
            allow: Vec::new(),
            deny: Vec::new(),
            timeout_secs: 120,
            output_max_bytes: 64 * 1024,
        }
    }
}

impl CommandPolicy {
    /// No shell programs may run — the shared preset behind every non-[`CodingMode::Normal`] mode.
    ///
    /// Reuses the existing allow-list rule in `coder-sandbox`: a **non-empty** `allow` list that
    /// matches nothing denies every command. Empty `allow` would mean "allow all", which is why
    /// this cannot simply be an empty list. The entry is a sentinel no command line can match.
    pub fn none_allowed() -> Self {
        Self {
            allow: vec!["!no-shell".into()],
            deny: Vec::new(),
            timeout_secs: 120,
            output_max_bytes: 64 * 1024,
        }
    }
}

/// Tool names exposed in coding **explore** mode (read-only catalog filter).
///
/// Write/mutate tools stay registered in the full catalog but are omitted here so the model is not
/// invited to call them. Enforcement still lives in [`PathPolicy`] / [`CommandPolicy`].
pub const EXPLORE_TOOL_NAMES: &[&str] = &[
    "list_files",
    "search_text",
    "read_file",
    "git_status",
    "git_diff",
];

/// System instructions for a coding explore session.
pub const EXPLORE_MODE_CODER_PROMPT: &str = "\
You are Liberado's read-only coding explorer. Inspect the codebase with list_files, search_text, \
read_file, git_status, and git_diff only. Do NOT edit files, apply patches, commit, push, or run \
shell commands. When you have enough context, call submit_report with a concise findings summary \
(relevant paths, how the code is structured, and anything the parent agent needs to act).";

/// System instructions injected when a coding session runs in plan mode.
///
/// Kept next to the policy helpers so pack and surfaces do not each invent plan-mode prose.
pub const PLAN_MODE_CODER_PROMPT: &str = "\
You are Liberado's coding planner (plan mode). Explore the codebase with read-only tools, then \
write a clear implementation plan ONLY to `.liberado/plan.md`. \
Do NOT edit any other files. Do NOT run shell commands, git commits, or apply patches outside that path. \
When the plan is written, call submit_report summarizing the plan and key risks.";

/// Which capability tier a coding session runs under.
///
/// Modes are **presets over the existing [`PathPolicy`] / [`CommandPolicy`] types**, not a second
/// permission system — `coding-tui-plan` calls them capability/path tiers, not different agents.
/// One enum rather than a `plan_mode` and an `explore_mode` bool because the tiers are mutually
/// exclusive: a pair of booleans makes "both set" representable, and then every consumer has to
/// invent the same precedence rule independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingMode {
    /// Full write access and the operator's normal command policy.
    #[default]
    Normal,
    /// Writes restricted to [`PLAN_ARTIFACT_REL`]; no shell.
    Plan,
    /// No writes at all, no shell, read-only tool catalog.
    Explore,
}

impl CodingMode {
    /// Parse the wire spellings a surface may send: `mode: "plan"` / `"explore"`, or the older
    /// `plan_mode` / `explore_mode` booleans. Returns `None` when the value names no mode, so a
    /// caller can fall through to the next source rather than defaulting prematurely.
    pub fn from_payload(root: &serde_json::Value) -> Option<Self> {
        if let Some(m) = root.get("mode").and_then(|v| v.as_str()) {
            if m.eq_ignore_ascii_case("plan") {
                return Some(Self::Plan);
            }
            if m.eq_ignore_ascii_case("explore") {
                return Some(Self::Explore);
            }
            if m.eq_ignore_ascii_case("normal") {
                return Some(Self::Normal);
            }
        }
        // Explore is the stricter tier, so it wins if a caller somehow sets both booleans.
        if root
            .get("explore_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Some(Self::Explore);
        }
        if root
            .get("plan_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Some(Self::Plan);
        }
        None
    }

    /// The write-path preset for this mode.
    pub fn path_policy(&self) -> PathPolicy {
        match self {
            Self::Normal => PathPolicy::default(),
            Self::Plan => PathPolicy::plan_mode(),
            Self::Explore => PathPolicy::read_only(),
        }
    }

    /// The command preset for this mode. Only [`CodingMode::Normal`] may shell out.
    pub fn command_policy(&self) -> CommandPolicy {
        match self {
            Self::Normal => CommandPolicy::default(),
            Self::Plan | Self::Explore => CommandPolicy::none_allowed(),
        }
    }

    /// Fixed worker prompt for the restricted tiers; `None` means "use the caller's prompt".
    pub fn coder_prompt(&self) -> Option<&'static str> {
        match self {
            Self::Normal => None,
            Self::Plan => Some(PLAN_MODE_CODER_PROMPT),
            Self::Explore => Some(EXPLORE_MODE_CODER_PROMPT),
        }
    }

    /// True for every tier that is not [`CodingMode::Normal`] — the ones that force their own
    /// policies and so must not be overridden by payload `path_policy` / `command_policy`.
    pub fn is_restricted(&self) -> bool {
        !matches!(self, Self::Normal)
    }

    /// How much this tier denies; higher is stricter.
    fn restriction_rank(&self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Plan => 1,
            Self::Explore => 2,
        }
    }

    /// Combine two sources fail-closed: whichever names the stricter tier wins.
    ///
    /// Restriction only ever accumulates. A profile that forces `plan` cannot be talked back down
    /// to `normal` by a goal payload, and a payload asking for `explore` still narrows a profile
    /// that only asked for `plan` — the same "neither source can disable what the other set" rule
    /// the single-mode presets shipped with, now with a defined answer when the tiers differ.
    pub fn strictest(a: Self, b: Self) -> Self {
        if b.restriction_rank() > a.restriction_rank() {
            b
        } else {
            a
        }
    }
}

/// A configured command the backend can expose through `validate` and run as a deterministic gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoderCommandConfig {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_max_bytes: Option<usize>,
}

impl CoderCommandConfig {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            timeout_secs: None,
            output_max_bytes: None,
        }
    }
}

/// Relative workspace path of the **only** file plan mode may write.
///
/// Plan mode reuses [`PathPolicy::allow_write_globs`] — it is not a second permission system.
/// Surfaces and packs that need a stable plan artifact path should use this constant rather than
/// inventing a parallel location.
pub const PLAN_ARTIFACT_REL: &str = ".liberado/plan.md";

/// Path containment and write policy for the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathPolicy {
    #[serde(default)]
    pub allow_write_globs: Vec<String>,
    #[serde(default)]
    pub deny_globs: Vec<String>,
    pub read_max_bytes: usize,
    pub search_max_results: usize,
}

impl Default for PathPolicy {
    fn default() -> Self {
        Self {
            allow_write_globs: vec!["**".to_string()],
            deny_globs: vec![
                ".git/**".to_string(),
                "target/**".to_string(),
                "node_modules/**".to_string(),
            ],
            read_max_bytes: 128 * 1024,
            search_max_results: 200,
        }
    }
}

impl PathPolicy {
    /// No writes under the workspace (coding explore / read-only subagent).
    ///
    /// Reuses the existing write-glob check in `coder-tools`: an empty `allow_write_globs` list
    /// matches nothing, so every write is denied. Not a second permission system.
    pub fn read_only() -> Self {
        Self {
            allow_write_globs: Vec::new(),
            ..Self::default()
        }
    }

    /// True when no write glob is allowed (explore mode and any future read-only preset).
    pub fn writes_disabled(&self) -> bool {
        self.allow_write_globs.is_empty()
    }

    /// Writes restricted to [`PLAN_ARTIFACT_REL`] only (coding plan mode).
    ///
    /// Reads still follow the usual deny list (`.git/**`, build dirs, …). Enforcement lives in
    /// `coder-tools` via the existing write-glob check — plan mode does not add a parallel gate.
    pub fn plan_mode() -> Self {
        Self {
            allow_write_globs: vec![PLAN_ARTIFACT_REL.to_string()],
            ..Self::default()
        }
    }
}

/// One configurable model/prompt role in a coder run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoderRoleConfig {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
}

/// Completion-gate settings for a coder run (S1 of `docs/roadmap/coding-tui-plan.md`).
///
/// The gate replaces the single-critic check with a remembered gatekeeper plus a quorum of cold
/// reviewers, adjudicated by `liberado_session::CompletionGate`. Every reviewer reuses the
/// `[critic]` role config unless overridden here, so turning the gate on does not require
/// re-declaring a model.
///
/// **Default off.** Unlike chat compaction — where an opt-in reliability guard is off in practice
/// and that was the argument for defaulting it on — this one multiplies review cost by
/// `1 + fresh_reviewers` model calls per attempt, on every attempt. It stays opt-in until the eval
/// curriculum has run against it (S7 in the plan), which is what turns "seems stricter" into a
/// measured accuracy number worth paying for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CoderGateConfig {
    /// Master switch. When false the legacy single-critic path runs unchanged.
    pub enabled: bool,
    /// Cold reviewers in the quorum. A strict majority must approve.
    pub fresh_reviewers: u8,
    /// Consecutive refuted attempts before the strategist proposes a structural change.
    /// 0 disables the strategist.
    pub strategist_after: u32,
    /// Role override for the remembered gatekeeper. Falls back to `[critic]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gatekeeper: Option<CoderRoleConfig>,
    /// Role override shared by every cold reviewer. Falls back to `[critic]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fresh: Option<CoderRoleConfig>,
    /// Role override for the strategist. Falls back to `[critic]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategist: Option<CoderRoleConfig>,
}

impl Default for CoderGateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            fresh_reviewers: 2,
            strategist_after: 3,
            gatekeeper: None,
            fresh: None,
            strategist: None,
        }
    }
}

/// Progress-loop thresholds. Values come from config; these defaults are only code-owned fallbacks.
///
/// Both limits count **tool calls, not turns** (`read_only_turn_limit` is misnamed), and each
/// escalates twice: a one-time nudge at the limit, then a latched fatal at 2×. So the operative
/// ceilings are double the numbers below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressPolicy {
    pub read_only_turn_limit: u32,
    pub same_tool_limit: u32,
    pub validation_repeat_limit: u32,
    pub max_attempts: u32,
    pub event_preview_max_chars: usize,
}

impl Default for ProgressPolicy {
    fn default() -> Self {
        Self {
            // Was 4 (fatal at 8). Eight inspect calls is not enough to understand a change that
            // spans crates: wiring one config field from `config-loader` through `server` into
            // `daemon` meant reading five files and searching for their call sites, and the guard
            // latched partway through. The model then filed a complete implementation plan it had
            // no remaining budget to carry out — twice, in two independent runs, reporting
            // "blocked from making edits by the progress guard". Exploration is the cheap part of
            // a coding task; starving it does not produce edits, it produces plans.
            //
            // Runaway repetition is still caught, and caught better, by the executor's args-aware
            // `is_doom_loop`/`detect_short_cycle` guards — those fire on calls that *repeat*, which
            // is the actual pathology. This limit only needs to bound the pathological case those
            // miss, so it can afford real headroom.
            read_only_turn_limit: 20,
            // Was 3 (fatal at 6). Reading six files in a row is ordinary orientation, not churn,
            // and parallel batches count each call separately — a single batched read of six files
            // tripped this on its own.
            same_tool_limit: 10,
            validation_repeat_limit: 2,
            max_attempts: 3,
            event_preview_max_chars: 500,
        }
    }
}

/// Hashline (line-anchored) edit mode for the coding harness.
///
/// Ported from oh-my-pi's hashline dialect: reads emit `[path#TAG]` content-hash headers and
/// `LINE:content` rows; the `hashline_edit` tool applies `PUT`/`CUT`/`REM` patches that bind to
/// those tags so stale anchors fail closed instead of corrupting files.
///
/// **Default off.** Enable via `[coder.hashline] enabled = true` in `tuning.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HashlineConfig {
    /// Master switch. When false, `read_file` returns plain content and `hashline_edit` is absent.
    pub enabled: bool,
    /// Length of the content-hash tag in characters (inclusive range 4–10).
    ///
    /// Tags are uppercase base-36 (`0-9A-Z`) fingerprints of the whole file's normalized text.
    pub hash_length: u8,
}

impl Default for HashlineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hash_length: 4,
        }
    }
}

impl HashlineConfig {
    /// Minimum allowed hash length (characters).
    pub const HASH_LENGTH_MIN: u8 = 4;
    /// Maximum allowed hash length (characters).
    pub const HASH_LENGTH_MAX: u8 = 10;

    /// Validate load-time constraints (hash length bounds when enabled or always, for fail-fast).
    pub fn validate(&self) -> Result<(), String> {
        if !(Self::HASH_LENGTH_MIN..=Self::HASH_LENGTH_MAX).contains(&self.hash_length) {
            return Err(format!(
                "hashline.hash_length must be between {} and {} (got {})",
                Self::HASH_LENGTH_MIN,
                Self::HASH_LENGTH_MAX,
                self.hash_length
            ));
        }
        Ok(())
    }
}

/// Fully resolved settings for one backend run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoderRunConfig {
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_dir: Option<String>,
    /// Which trace formats to write. Empty means native only — see [`TraceFormat`].
    #[serde(default)]
    pub trace_formats: Vec<TraceFormat>,
    pub planner: CoderRoleConfig,
    pub coder: CoderRoleConfig,
    pub critic: CoderRoleConfig,
    /// Completion gate (S1). Absent table = off; the single-critic path runs.
    #[serde(default)]
    pub gate: CoderGateConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<CoderRoleConfig>,
    #[serde(default)]
    pub sandbox: SandboxSpec,
    #[serde(default)]
    pub command_policy: CommandPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_command: Option<CoderCommandConfig>,
    /// Ordered harness checks (see `docs/architecture/verifiers.md`). When empty, a single
    /// `validation_command` is still honored as a legacy one-entry pipeline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifiers: Vec<VerifierSpec>,
    #[serde(default)]
    pub verify_policy: PipelinePolicy,
    #[serde(default)]
    pub path_policy: PathPolicy,
    #[serde(default)]
    pub progress: ProgressPolicy,
    /// Hashline edit mode (`[coder.hashline]`). Default off.
    #[serde(default)]
    pub hashline: HashlineConfig,
}

/// Input to a coding backend. PR-factory details such as pushing branches and opening PRs stay
/// outside this request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoderRunRequest {
    pub task: CoderTask,
    pub workspace: WorkspaceRef,
    pub config: CoderRunConfig,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub prior_feedback: Vec<String>,
    /// One structural change proposed by the completion gate's strategist after repeated
    /// refutations (`CoderGateConfig::strategist_after`).
    ///
    /// A first-class field rather than another `prior_feedback` entry on purpose: prior feedback is
    /// rendered on retries through `repair_focus_block`, which shows only the *last* entry in full
    /// and truncates the rest to one line each. A directive pushed in there would be either
    /// mislabelled as "Latest failure detail" or silently clipped — and a structural instruction
    /// that arrives clipped is worse than none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategist_directive: Option<String>,
}

/// Terminal output from a coding backend before the PR factory commits/pushes/opens a PR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoderRunResult {
    pub backend: String,
    pub outcome: Outcome,
    pub summary: String,
    #[serde(default)]
    pub files_changed: Vec<String>,
    /// The same files with their change kind, for the `file_changed` wire event. Kept alongside
    /// `files_changed` rather than replacing it: that field is the session's *artifact* list and
    /// several callers treat it as plain paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_changes: Vec<FileChangeRecord>,
    #[serde(default)]
    pub validation_notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critic_verdict: Option<CriticVerdict>,
    /// Individual completion-gate reviewer votes behind `critic_verdict`, in casting order.
    /// Empty when the gate is disabled. Carried on the result so the session pack can put them on
    /// the wire — the backend has no `SessionEvent` sender of its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_votes: Vec<GateVoteRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_path: Option<String>,
    #[serde(default)]
    pub diagnostics: serde_json::Value,
}

impl CoderRunResult {
    pub fn report(&self) -> Report {
        Report {
            outcome: self.outcome,
            summary: self.summary.clone(),
            artifacts: self.files_changed.clone(),
            new_high_signal_facts: Vec::new(),
            follow_up: None,
            deferred_to_human: false,
            repeat_calls: 0,
        }
    }
}

/// One changed workspace file and how it changed (`added` | `modified` | `deleted`).
/// Workspace-relative path — never an absolute host path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeRecord {
    pub path: String,
    pub change: String,
}

/// One completion-gate reviewer's vote, flattened for transport.
///
/// Plain data on purpose: it mirrors `liberado_session::RecordedVote` without making this crate
/// depend on the session kernel, so `coder-core` stays the narrow contract waist it claims to be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateVoteRecord {
    pub reviewer: String,
    /// `gatekeeper` | `fresh` | `strategist`.
    pub kind: String,
    pub approved: bool,
    #[serde(default)]
    pub issues: Vec<String>,
    /// The gate substituted this vote because the reviewer failed — not a real rejection.
    #[serde(default)]
    pub coerced: bool,
}

/// A diff-reviewing critic's verdict over the actual workspace diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "quality", rename_all = "snake_case")]
pub enum CriticVerdict {
    Acceptable,
    NeedsRevision { issues: Vec<String> },
}

/// Stable event stream for PR dispatch logs and future TUI/CLI rendering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoderEvent {
    SessionStarted {
        session_id: String,
        backend: String,
        task_id: String,
        at: DateTime<Utc>,
    },
    RoleStarted {
        role: String,
        model: String,
        at: DateTime<Utc>,
    },
    RoleFinished {
        role: String,
        at: DateTime<Utc>,
    },
    ModelTurnStarted {
        role: String,
        turn: u32,
        at: DateTime<Utc>,
    },
    ModelTurnFinished {
        role: String,
        turn: u32,
        /// The tools **offered** on this turn, in catalog order.
        ///
        /// Guards withdraw tools as a run proceeds, so this changes turn to turn. Without it,
        /// answering "did the model even have `write_file`?" meant reading `catalog()` and
        /// `PathPolicy` and working out which mode was active — which is how four failed runs got
        /// misdiagnosed before anyone thought to check.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tools_offered: Vec<String>,
        /// How many messages the model was sent this turn.
        #[serde(default)]
        message_count: usize,
        /// The model's own text, verbatim and untruncated. `None` when it emitted only tool calls.
        ///
        /// Deliberately not run through `preview_str`: the 500-char cap exists to protect the event
        /// bus and the UI, and a trace file has no such constraint. Truncating the one field that
        /// explains the model's reasoning would defeat the point of recording it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        /// `"tool_calls"` or `"prose"` — why the turn ended.
        #[serde(default)]
        finish_reason: String,
        /// Tool names the model asked for, in call order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<String>,
        #[serde(default)]
        prompt_tokens: u32,
        #[serde(default)]
        completion_tokens: u32,
        at: DateTime<Utc>,
    },
    ToolStarted {
        name: String,
        args_preview: String,
        at: DateTime<Utc>,
    },
    ToolFinished {
        name: String,
        ok: bool,
        result_preview: String,
        at: DateTime<Utc>,
    },
    FileChanged {
        path: String,
        at: DateTime<Utc>,
    },
    ValidationFinished {
        ok: bool,
        summary: String,
        at: DateTime<Utc>,
    },
    LoopGuardTriggered {
        guard: String,
        action: String,
        at: DateTime<Utc>,
    },
    CriticVerdict {
        verdict: CriticVerdict,
        at: DateTime<Utc>,
    },
    ReportFiled {
        outcome: Outcome,
        summary: String,
        at: DateTime<Utc>,
    },
    SessionFinished {
        outcome: Outcome,
        at: DateTime<Utc>,
    },
}

/// Durable replay artifact for a coder session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoderTrace {
    pub session_id: String,
    pub request: CoderRunRequest,
    #[serde(default)]
    pub events: Vec<CoderEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CoderRunResult>,
}

/// Common error shape across coding backends.
#[derive(Debug, Error)]
pub enum CoderError {
    #[error("backend setup failed: {0}")]
    Setup(String),
    #[error("sandbox failed: {0}")]
    Sandbox(String),
    #[error("tool failed: {0}")]
    Tool(String),
    #[error("model/provider failed: {0}")]
    Provider(String),
    #[error("no real workspace changes were produced")]
    NoChanges,
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("backend failed: {0}")]
    Backend(String),
}

#[async_trait]
pub trait CoderBackend: Send + Sync {
    fn name(&self) -> &str;

    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(model: &str) -> CoderRoleConfig {
        CoderRoleConfig {
            model: model.to_string(),
            prompt_path: Some(format!("prompts/{model}.md")),
            prompt: None,
            temperature: Some(0.1),
            max_tokens: Some(4096),
            max_turns: Some(8),
        }
    }

    #[test]
    fn run_request_round_trips_json() {
        let request = CoderRunRequest {
            task: CoderTask::new("task-1", "add a copy button").with_context("webui chat"),
            workspace: WorkspaceRef::new("C:/repo", "main"),
            config: CoderRunConfig {
                backend: LIBERADO_LOOP_BACKEND.to_string(),
                trace_dir: Some("coder-traces".to_string()),
                trace_formats: Vec::new(),
                planner: role("deepseek/deepseek-v4-pro"),
                coder: role("deepseek/deepseek-v4-pro"),
                critic: role("deepseek/deepseek-v4-flash"),
                gate: CoderGateConfig::default(),
                repair: None,
                sandbox: SandboxSpec::HostLocal,
                command_policy: CommandPolicy::default(),
                verifiers: Vec::new(),
                verify_policy: PipelinePolicy::default(),
                validation_command: Some(CoderCommandConfig {
                    program: "cargo".to_string(),
                    args: vec!["test".to_string()],
                    env: std::collections::BTreeMap::new(),
                    timeout_secs: Some(300),
                    output_max_bytes: Some(4096),
                }),
                path_policy: PathPolicy::default(),
                progress: ProgressPolicy::default(),
                hashline: HashlineConfig::default(),
            },
            attempt: 0,
            prior_feedback: Vec::new(),
            strategist_directive: None,
        };

        let json = serde_json::to_string_pretty(&request).unwrap();
        let back: CoderRunRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, back);
    }

    #[test]
    fn sandbox_worktree_round_trips_json() {
        let spec = SandboxSpec::Worktree;
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(json, r#"{"backend":"worktree"}"#);
        let back: SandboxSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SandboxSpec::Worktree);
    }

    #[test]
    fn coder_result_converts_to_report() {
        let result = CoderRunResult {
            backend: LIBERADO_LOOP_BACKEND.to_string(),
            outcome: Outcome::Succeeded,
            summary: "Added copy button".to_string(),
            files_changed: vec!["crates/webui/src/components/chat.rs".to_string()],
            file_changes: Vec::new(),
            validation_notes: Some("cargo check passed".to_string()),
            critic_verdict: Some(CriticVerdict::Acceptable),
            gate_votes: Vec::new(),
            trace_path: Some("traces/task-1.jsonl".to_string()),
            diagnostics: serde_json::json!({"turns": 5}),
        };

        let report = result.report();
        assert_eq!(report.outcome, Outcome::Succeeded);
        assert_eq!(report.artifacts, result.files_changed);
        assert!(report.summary.contains("copy button"));
    }

    #[test]
    fn gate_config_default_is_disabled() {
        let gate = CoderGateConfig::default();
        assert!(!gate.enabled);
        assert_eq!(gate.fresh_reviewers, 2);
        assert_eq!(gate.strategist_after, 3);
        assert!(gate.gatekeeper.is_none());
        assert!(gate.fresh.is_none());
        assert!(gate.strategist.is_none());
    }

    #[test]
    fn hashline_config_default_is_disabled_with_length_4() {
        let h = HashlineConfig::default();
        assert!(!h.enabled);
        assert_eq!(h.hash_length, 4);
        assert!(h.validate().is_ok());
    }

    #[test]
    fn hashline_config_rejects_out_of_range_length() {
        assert!(
            HashlineConfig {
                enabled: true,
                hash_length: 3,
            }
            .validate()
            .is_err()
        );
        assert!(
            HashlineConfig {
                enabled: false,
                hash_length: 11,
            }
            .validate()
            .is_err()
        );
        assert!(
            HashlineConfig {
                enabled: true,
                hash_length: 10,
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn hashline_config_accepts_every_length_in_range() {
        for len in HashlineConfig::HASH_LENGTH_MIN..=HashlineConfig::HASH_LENGTH_MAX {
            assert!(
                HashlineConfig {
                    enabled: true,
                    hash_length: len,
                }
                .validate()
                .is_ok(),
                "length {len}"
            );
        }
    }

    #[test]
    fn hashline_config_round_trips_json() {
        let cfg = HashlineConfig {
            enabled: true,
            hash_length: 7,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: HashlineConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn coder_run_config_deserializes_absent_hashline_as_default() {
        // Minimal JSON without hashline key — serde default must fill it.
        let json = r#"{
            "backend": "liberado-loop",
            "planner": {"model": "m", "prompt": "p", "max_turns": 1},
            "coder": {"model": "m", "prompt": "p", "max_turns": 1},
            "critic": {"model": "m", "prompt": "p", "max_turns": 1}
        }"#;
        let cfg: CoderRunConfig = serde_json::from_str(json).unwrap();
        assert!(!cfg.hashline.enabled);
        assert_eq!(cfg.hashline.hash_length, 4);
    }

    #[test]
    fn plan_mode_coder_prompt_is_non_empty() {
        assert!(!PLAN_MODE_CODER_PROMPT.is_empty());
        assert!(PLAN_MODE_CODER_PROMPT.contains(".liberado/plan.md"));
    }

    #[test]
    fn explore_mode_coder_prompt_is_non_empty() {
        assert!(!EXPLORE_MODE_CODER_PROMPT.is_empty());
        assert!(EXPLORE_MODE_CODER_PROMPT.contains("read-only"));
    }

    #[test]
    fn explore_tool_names_are_write_free() {
        assert!(EXPLORE_TOOL_NAMES.contains(&"list_files"));
        assert!(EXPLORE_TOOL_NAMES.contains(&"read_file"));
        assert!(!EXPLORE_TOOL_NAMES.contains(&"write_file"));
        assert!(!EXPLORE_TOOL_NAMES.contains(&"edit_file"));
    }

    #[test]
    fn command_policy_none_allowed_denies_everything() {
        let p = CommandPolicy::none_allowed();
        assert!(
            !p.allow.is_empty(),
            "non-empty allow list with sentinel blocks all commands"
        );
        assert_eq!(p.output_max_bytes, 64 * 1024);
        assert_eq!(p.timeout_secs, 120);
    }

    #[test]
    fn path_policy_plan_mode_restricts_to_plan_artifact() {
        let p = PathPolicy::plan_mode();
        assert_eq!(p.allow_write_globs, vec![PLAN_ARTIFACT_REL]);
        assert!(!p.writes_disabled());
    }

    #[test]
    fn path_policy_read_only_disables_all_writes() {
        let p = PathPolicy::read_only();
        assert!(p.allow_write_globs.is_empty());
        assert!(p.writes_disabled());
    }

    #[test]
    fn path_policy_writes_disabled_when_no_globs() {
        let mut p = PathPolicy::default();
        assert!(!p.writes_disabled());
        p.allow_write_globs.clear();
        assert!(p.writes_disabled());
    }

    #[test]
    fn path_policy_writes_not_disabled_when_globs_present() {
        let p = PathPolicy::default();
        assert!(!p.writes_disabled());
    }
}
