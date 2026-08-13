//! # liberado-coder-core
//!
//! Provider-agnostic contracts for Liberado's Rust-native coding backend. This crate intentionally
//! owns no model loop, filesystem mutation, forge API, or sandbox implementation. It is the narrow
//! waist between PR production, a future TUI/CLI coding surface, eval/tuning harnesses, and the
//! Liberado loop backend.
//!
//! Also hosts **verifier** and **criteria-intake** DTOs (`verify`, `intake`) — domain-agnostic shapes
//! first consumed by the coding pack; see `docs/spec/architecture/verifiers.md`.

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
pub mod prompts;
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
/// (`docs/spec/architecture/agentic-loops.md` §Vocabulary — this backend runs *goals*, not *loops*).
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
            deny: vec!["git".to_string()],
            // `git` is denied because the dedicated git tools (git_branch / git_commit / git_push
            // / git_status / git_diff / git_log / git_fetch / git_merge) are the only sanctioned
            // path to git, and they run through the gix library, not a shell. An empty `allow`
            // list means "allow all", so without this deny entry `run_command` could invoke git
            // with no capability check at all — backlog item C1. A library call is something the
            // capability model can see; an allow-listed shell is a hole in it.
            //
            // 120s axed every workspace-wide cargo command on a cold worktree. One run opened
            // with `cargo test --workspace`, hit the ceiling, got back nothing at all, and
            // retried the same command three more times — most of a five-minute run spent on a
            // build that was never allowed to finish. A model cannot learn from a timeout that
            // reports no output.
            //
            // Turn limits and the run budget are the real bound on a wasteful command; this
            // ceiling only needs to be longer than an honest build.
            timeout_secs: 900,
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
pub const EXPLORE_TOOL_NAMES: &[&str] =
    &["list_files", "grep", "read_file", "git_status", "git_diff"];

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
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
    /// Reasoning / thinking effort (`off` / `low` / `medium` / `high`). Mapped onto the
    /// OpenAI-compatible `reasoning` body field. `None` leaves the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// Completion-gate settings for a coder run (S1 of `docs/future-work/coding-tui-plan.md`).
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

/// Where a coding run builds, and whether the harness warms it first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceBuildConfig {
    /// One `CARGO_TARGET_DIR` shared by every coding worktree.
    ///
    /// A worktree starts with no build cache, so the run's first compile rebuilds the whole
    /// dependency graph — 706 of this workspace's 770 packages are registry crates, and those are
    /// keyed by registry/name/version rather than by path, so every worktree can reuse them.
    /// Workspace-member crates live at different paths and get their own artifacts, which coexist
    /// rather than collide.
    ///
    /// **One run at a time.** Cargo takes an exclusive lock on a target directory; concurrent
    /// builds do not corrupt each other, they queue. Measured: a second `cargo build` printed
    /// "Blocking waiting for file lock on artifact directory" and waited out the first. With a
    /// command timeout in play, a run queued behind a cold build times out having done nothing,
    /// which is worse than giving it its own cache. Sharing safely across concurrent runs needs
    /// a lock-free compiler cache such as `sccache`, not this.
    ///
    /// Unset means each worktree keeps its own `target/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_target_dir: Option<String>,
    /// Build the workspace once, before the model is given anything.
    ///
    /// Two reasons, and the second is the expensive one.
    ///
    /// It proves the baseline compiles. Two runs were diagnosed as the model writing broken code
    /// when nobody had checked whether the worktree built to begin with; it did, but the question
    /// should not have needed a trace to answer.
    ///
    /// And it keeps the provider's prompt cache warm. Send the system prompt, then make the model
    /// wait minutes for a cold build, and the cached prefix has expired by the time the next
    /// message goes out — so those tokens are paid for twice. Building first means every token
    /// the run sends is sent close together.
    #[serde(default = "default_warmup")]
    pub warmup: bool,
    /// Ceiling for the warm-up build. Generous on purpose: a cold build of this workspace is
    /// minutes, and the whole point is to pay that cost once, before the model is listening.
    #[serde(default = "default_warmup_timeout")]
    pub warmup_timeout_secs: u64,
}

fn default_warmup() -> bool {
    true
}

fn default_warmup_timeout() -> u64 {
    1800
}

impl Default for WorkspaceBuildConfig {
    fn default() -> Self {
        Self {
            shared_target_dir: None,
            warmup: default_warmup(),
            warmup_timeout_secs: default_warmup_timeout(),
        }
    }
}

/// How `edit_file` decides an anchor matches.
///
/// Separate from [`HashlineConfig`] because it governs a different tool: hashline anchors on a
/// line number and cannot be "nearly right", while `edit_file` anchors on text and routinely is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditConfig {
    /// Accept an anchor that is close enough when no exact match exists.
    ///
    /// On by default, following `oh-my-pi`, which ships `edit.fuzzyMatch: true`. Four dispatched
    /// runs here never got the anchor failure rate below 42%, and one failure was an anchor
    /// correct in every character except four leading spaces the file did not have.
    #[serde(default = "default_fuzzy_match")]
    pub fuzzy_match: bool,
    /// Similarity a candidate must reach, 0.0 to 1.0.
    ///
    /// 0.95 is `oh-my-pi`'s default and is reproduced rather than re-derived — their number is
    /// tuned against far more traffic than we have. Lowering it trades wrong-place edits for
    /// fewer rejections, which is the wrong direction: a rejected edit costs a turn, an edit in
    /// the wrong place is reported as success.
    #[serde(default = "default_fuzzy_threshold")]
    pub fuzzy_threshold: f64,
}

fn default_fuzzy_match() -> bool {
    true
}

fn default_fuzzy_threshold() -> f64 {
    EditConfig::DEFAULT_FUZZY_THRESHOLD
}

impl EditConfig {
    /// `oh-my-pi`'s default, and the single source for it.
    ///
    /// The matcher in `coder-tools` had its own copy of this number. Two constants meaning the
    /// same thing in two crates is the divergence that produced the hashline split, at a smaller
    /// scale — so the matcher reads this one.
    pub const DEFAULT_FUZZY_THRESHOLD: f64 = 0.95;
}

impl Default for EditConfig {
    fn default() -> Self {
        Self {
            fuzzy_match: default_fuzzy_match(),
            fuzzy_threshold: default_fuzzy_threshold(),
        }
    }
}

impl Default for HashlineConfig {
    /// **Off**, at length 7.
    ///
    /// It was flipped on in #105 to end a divergence between the two coding paths, on the
    /// reasoning that line anchors cannot be ambiguous. A four-run series then measured it: with
    /// hashline on, `read_file` returns `[path#TAG]` + `LINE:content` while `edit_file` matches
    /// raw text, and the model pasted the numbered view into the text tool in **14 of 41** calls.
    /// That run had the worst anchor failure rate of the four (72%); the same task with hashline
    /// off had the best (42%) and produced 159 insertions with no deletions.
    ///
    /// `oh-my-pi` has the same feature and does not have this problem, because its `edit.mode` is
    /// an enum: exactly one edit tool exists at a time. The catalog is now exclusive here too, so
    /// hashline is usable again — but off stays the default until a run measures it winning.
    ///
    /// It was off here, and the ACP path took the default while `coder-runner` opted in, so the
    /// tool built to make line-anchored edits unambiguous was missing from the path we dogfood
    /// through. A dispatched run then failed on exactly that: of 25 `edit_file` calls, 15 came
    /// back `old text was not found` or `old text matched 2 times; provide more context`, and the
    /// run was abandoned without landing a line. `hashline_edit` answers both errors by
    /// construction — `read_file` hands back `[path#TAG]` plus `LINE:content`, so the anchor is a
    /// line number rather than a string that may appear twice in a 3,800-line file.
    ///
    /// The default is changed rather than a second `enabled: true` added at the ACP call site,
    /// because the divergence *is* the bug. Three fixes this month landed on one of these two
    /// paths and not the other; adding a third literal would have set up the fourth.
    fn default() -> Self {
        Self {
            enabled: false,
            hash_length: 7,
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
    /// Ordered harness checks (see `docs/spec/architecture/verifiers.md`). When empty, a single
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
    /// Post-run honesty review (`[coder.session_critic]`). Default off.
    #[serde(default)]
    pub session_critic: SessionCriticConfig,
    /// Where to look for harness prompt files. See [`prompts`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_dir: Option<String>,
    /// Anchor matching for `edit_file` (`[coder.edit]`).
    #[serde(default)]
    pub edit: EditConfig,
    /// Build cache and warm-up (`[coder.workspace]`).
    #[serde(default)]
    pub workspace_build: WorkspaceBuildConfig,
    /// Names of coding tools to offer the model. `None` / empty = the full pack catalog.
    /// Executor finish tools (`submit_report`, scratchpad) are not this list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered_tools: Option<Vec<String>>,
}

// ── review findings ───────────────────────────────────────────────────────────────────────────

/// What would actually resolve a session-critic finding.
///
/// Not every honesty finding is a code defect, and treating them alike sends a coding agent to
/// rewrite a paragraph. The three real shapes, from the runs we have:
///
/// - a test that does not bind to the change it accompanies -> [`Remedy::Repair`]
/// - a mutation table for mutations that were never run -> [`Remedy::Verify`]. The code may be
///   perfectly good; what is missing is the evidence, so the remedy is to go and get it.
/// - a report that overclaims what was proven -> [`Remedy::Retract`], a text edit, no coding run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Remedy {
    /// Change the code or the tests.
    Repair,
    /// Run the check that was claimed, then act on what it says.
    Verify,
    /// Correct the report. No code change.
    Retract,
    /// Nothing to do; recorded for the reader.
    #[default]
    None,
}

impl Remedy {
    /// Whether a coding run could act on this. `Retract` and `None` cannot be coded away.
    pub fn is_actionable(self) -> bool {
        matches!(self, Remedy::Repair | Remedy::Verify)
    }
}

/// One thing a run said that does not survive contact with the rest of the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFinding {
    /// `abandoned_finding` | `unsupported_claim` | `silent_reversal`. A string rather than an
    /// enum: an unexpected value from the reviewer is information, and folding it into `Other`
    /// throws that information away.
    pub kind: String,
    /// The run's own words, verbatim. A finding without a quote cannot be checked by the person
    /// reading it, and an unfalsifiable review is worse than none.
    pub quote: String,
    /// Why those words conflict with the rest of the run.
    pub why: String,
    #[serde(default)]
    pub remedy: Remedy,
}

/// Verdict of the session critic. Empty `findings` is the ordinary result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionReview {
    #[serde(default)]
    pub findings: Vec<SessionFinding>,
}

impl SessionReview {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
    /// Findings a coding run could act on, in report order.
    pub fn actionable(&self) -> Vec<&SessionFinding> {
        self.findings
            .iter()
            .filter(|f| f.remedy.is_actionable())
            .collect()
    }
}

/// What became of a diff-critic issue across the run's attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Raised on one attempt, absent from the next. The implementer acted on it.
    Fixed,
    /// Still standing when the run ended. This is the one that must not be buried.
    Outstanding,
    /// The implementer argued the finding was wrong.
    ///
    /// Never produced yet: saying so requires a channel from the model back to the reviewer, and
    /// that channel does not exist. The variant is here so the *renderer* is written for the
    /// world we want rather than retrofitted into it — but nothing sets it, and a reader seeing
    /// only `fixed` and `outstanding` today is seeing the truth.
    Disputed,
}

/// A diff-critic issue plus what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFinding {
    pub issue: String,
    pub disposition: Disposition,
    /// Attempt index (0-based) the issue was first raised on.
    #[serde(default)]
    pub first_seen_attempt: u32,
}

/// What a remediation run did, if one was allowed to happen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemediationRecord {
    /// Branch the fix was written on. Never the implementer's branch.
    pub branch: String,
    pub outcome: Outcome,
    pub summary: String,
    /// The findings it was asked to address, in the order they were given.
    #[serde(default)]
    pub addressed: Vec<String>,
}

/// `[coder.session_critic]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCriticConfig {
    /// Off by default. On, it reads the run's narration after every attempt has finished.
    #[serde(default)]
    pub enabled: bool,
    /// Reviewer role. Falls back to `[critic]` — the same fallback the gate's reviewers use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<CoderRoleConfig>,
    /// Include the names of tools the run called. Measured: dropping them cost two of four
    /// labelled traces and produced a false accusation built out of the missing information.
    #[serde(default = "default_true")]
    pub include_tool_names: bool,
    /// Spawn a cold coding run to fix actionable findings, on its own branch.
    ///
    /// **Off by default and it should stay off until precision is measured.** A ready-made fix is
    /// an argument for the finding that produced it: a reviewer looking at a working diff is far
    /// likelier to take it than to go back and check whether the allegation was ever true. When
    /// the reviewer is wrong, this converts a cheap false positive into a plausible wrong change.
    #[serde(default)]
    pub remediation: bool,
}

fn default_true() -> bool {
    true
}

impl Default for SessionCriticConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            role: None,
            include_tool_names: true,
            remediation: false,
        }
    }
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
    /// Diff-critic issues with what became of each. Empty when the gate is off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diff_findings: Vec<DiffFinding>,
    /// Session-critic findings. Empty when the critic is off or found nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_findings: Vec<SessionFinding>,
    /// The remediation run, when one was allowed and had something to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<RemediationRecord>,
    #[serde(default)]
    pub diagnostics: serde_json::Value,
}

/// Render every finding for a human, findings first.
///
/// Ordering is the whole mechanism. A run that fixed four issues and deferred one must not read
/// as a clean run with a footnote — the outstanding item is the reason anybody is reading. So
/// outstanding diff issues lead, session findings follow, and anything already resolved goes last
/// where it belongs.
///
/// Returns an empty string when there is nothing to say, so a caller can append it unconditionally
/// without producing an empty heading.
pub fn render_findings_markdown(result: &CoderRunResult) -> String {
    let outstanding: Vec<&DiffFinding> = result
        .diff_findings
        .iter()
        .filter(|f| f.disposition != Disposition::Fixed)
        .collect();
    let fixed = result.diff_findings.len() - outstanding.len();

    if outstanding.is_empty() && result.session_findings.is_empty() && fixed == 0 {
        return String::new();
    }

    let mut out = String::from("## Review findings\n");

    if !outstanding.is_empty() {
        out.push_str("\n### Open — from the diff review\n\n");
        for f in &outstanding {
            let label = match f.disposition {
                Disposition::Disputed => "disputed by the implementer",
                _ => "not addressed",
            };
            out.push_str(&format!("- **{label}** — {}\n", f.issue));
        }
    }

    if !result.session_findings.is_empty() {
        out.push_str("\n### Open — from the session review\n\n");
        out.push_str(
            "The reviewer read the run's own narration. Each finding quotes the run verbatim; \
             check the quote before acting on it.\n\n",
        );
        for f in &result.session_findings {
            out.push_str(&format!("- **{}** ({:?}) — {}\n", f.kind, f.remedy, f.why));
            out.push_str(&format!("  > {}\n", f.quote.replace('\n', " ")));
        }
    }

    if let Some(remediation) = &result.remediation {
        out.push_str(&format!(
            "\n### A speculative fix exists\n\n\
             Branch `{}` ({:?}) was written by a cold agent from the findings above. \
             **The findings are unverified** — read them first and judge them on their own; the \
             existence of a fix is not evidence that the fix was needed.\n\n{}\n",
            remediation.branch, remediation.outcome, remediation.summary
        ));
    }

    if fixed > 0 {
        out.push_str(&format!(
            "\n### Closed\n\n{fixed} diff-review issue(s) were raised and addressed during the run.\n"
        ));
    }
    out
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
    /// What the model was sent, recorded **before** the call.
    ///
    /// [`CoderEvent::ModelTurnFinished`] describes the response. This describes the request, and
    /// exists because the difference between two harnesses on the same task came down to what
    /// each told the model — which neither trace recorded.
    ModelRequestSent {
        role: String,
        turn: u32,
        /// Tools offered on this request, in catalog order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tools_offered: Vec<String>,
        #[serde(default)]
        message_count: usize,
        /// Lowercase hex SHA-256 of the system message as sent. Present every turn, so a prompt
        /// that changes mid-run is visible as a changed hash.
        system_prompt_sha256: String,
        /// The system message verbatim, recorded the first time each distinct hash is seen.
        ///
        /// Not every turn: a 5 KB prompt across forty turns is 200 KB of the same text. Once per
        /// distinct hash is the same information — the hash on every turn says whether it is
        /// still that text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
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
    /// The attempt ended on an error nothing along the way expected.
    ///
    /// Distinct from `SessionFinished { outcome: Failed }`, which is a *decision* — a verifier
    /// refused, a critic asked for revision, nothing changed. This is the absence of a decision:
    /// something returned `Err` and unwound. The two look identical in a summary and could not be
    /// less alike to debug, so they are not the same event.
    ///
    /// Carries the error text because "the attempt failed" without the reason is exactly the state
    /// that made four consecutive failures cost a day each.
    SessionAborted {
        error: String,
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
            reasoning: None,
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
                session_critic: SessionCriticConfig::default(),
                prompt_dir: None,
                edit: Default::default(),
                workspace_build: Default::default(),
                offered_tools: None,
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
            diff_findings: Vec::new(),
            session_findings: Vec::new(),
            remediation: None,
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

    /// The default was `enabled: false, hash_length: 4`. It is now on at 7 — see
    /// `HashlineConfig::default` for the run that changed it. The values live in
    /// `hashline_default_tests`, which says *why* each one is what it is; this test keeps only
    /// the part that belongs here: whatever the default is, it must pass its own validator.
    #[test]
    fn the_default_hashline_config_validates() {
        assert!(HashlineConfig::default().validate().is_ok());
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
        // An absent `[coder.hashline]` must land on the *same* default a caller gets from
        // `HashlineConfig::default()`. Comparing against the type, not against literals, is what
        // stops this test from having to be edited every time the default is retuned — and it is
        // the property the test is actually about: deserialization must not invent its own.
        assert_eq!(cfg.hashline, HashlineConfig::default());
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

#[cfg(test)]
mod findings_tests {
    use super::*;
    use std::path::Path;

    fn result_with(diff: Vec<DiffFinding>, session: Vec<SessionFinding>) -> CoderRunResult {
        CoderRunResult {
            backend: "t".into(),
            outcome: Outcome::Succeeded,
            summary: "done".into(),
            files_changed: Vec::new(),
            file_changes: Vec::new(),
            validation_notes: None,
            critic_verdict: None,
            gate_votes: Vec::new(),
            trace_path: None,
            diff_findings: diff,
            session_findings: session,
            remediation: None,
            diagnostics: serde_json::Value::Null,
        }
    }

    fn diff(issue: &str, disposition: Disposition) -> DiffFinding {
        DiffFinding {
            issue: issue.into(),
            disposition,
            first_seen_attempt: 0,
        }
    }

    fn session(kind: &str, remedy: Remedy) -> SessionFinding {
        SessionFinding {
            kind: kind.into(),
            quote: "the mutation test passes even when I break run_headless".into(),
            why: "shipped it anyway".into(),
            remedy,
        }
    }

    /// Only `Repair` and `Verify` can be handed to a coding agent. Sending one to fix a paragraph
    /// spends a whole run on a text edit.
    #[test]
    fn only_code_shaped_remedies_are_actionable() {
        assert!(Remedy::Repair.is_actionable());
        assert!(Remedy::Verify.is_actionable());
        assert!(!Remedy::Retract.is_actionable());
        assert!(!Remedy::None.is_actionable());
    }

    #[test]
    fn actionable_filters_the_review() {
        let review = SessionReview {
            findings: vec![
                session("abandoned_finding", Remedy::Repair),
                session("unsupported_claim", Remedy::Retract),
                session("silent_reversal", Remedy::Verify),
            ],
        };
        let kinds: Vec<&str> = review
            .actionable()
            .iter()
            .map(|f| f.kind.as_str())
            .collect();
        assert_eq!(kinds, vec!["abandoned_finding", "silent_reversal"]);
    }

    /// Nothing to report must render as nothing, not as an empty heading a reader has to scan.
    #[test]
    fn a_clean_run_renders_empty() {
        assert!(render_findings_markdown(&result_with(Vec::new(), Vec::new())).is_empty());
    }

    /// The ordering *is* the mechanism. A run that fixed four issues and left one open must not
    /// read as a clean run with a footnote — the open item is why anyone is reading.
    #[test]
    fn open_findings_come_before_closed_ones() {
        let rendered = render_findings_markdown(&result_with(
            vec![
                diff("cosmetic thing", Disposition::Fixed),
                diff("the test does not bind", Disposition::Outstanding),
            ],
            Vec::new(),
        ));
        let open = rendered
            .find("the test does not bind")
            .expect("open issue shown");
        let closed = rendered.find("### Closed").expect("closed section shown");
        assert!(
            open < closed,
            "an outstanding finding must not sit below the resolved ones:\n{rendered}"
        );
    }

    /// A resolved issue must not be presented as open. Crying wolf on fixed work is how a reader
    /// learns to skip the section.
    #[test]
    fn a_fixed_issue_is_not_reported_as_open() {
        let rendered = render_findings_markdown(&result_with(
            vec![diff("gone now", Disposition::Fixed)],
            Vec::new(),
        ));
        let open_section = rendered.split("### Closed").next().unwrap_or("");
        assert!(
            !open_section.contains("gone now"),
            "a fixed issue appeared above the Closed heading:\n{rendered}"
        );
    }

    /// Every session finding must carry its quote into the report. A finding a reader cannot
    /// check against the transcript is an accusation, not a review.
    #[test]
    fn session_findings_carry_their_quote() {
        let rendered = render_findings_markdown(&result_with(
            Vec::new(),
            vec![session("abandoned_finding", Remedy::Repair)],
        ));
        assert!(
            rendered.contains("passes even when I break run_headless"),
            "the verbatim quote is what makes the finding checkable:\n{rendered}"
        );
    }

    /// A speculative fix must be introduced as speculative. A reviewer shown a working diff is
    /// far likelier to take it than to go back and test whether the finding behind it was true.
    #[test]
    fn a_remediation_branch_is_labelled_unverified() {
        let mut result = result_with(
            Vec::new(),
            vec![session("abandoned_finding", Remedy::Repair)],
        );
        result.remediation = Some(RemediationRecord {
            branch: "agent/remediation-x".into(),
            outcome: Outcome::Succeeded,
            summary: "rewrote the test".into(),
            addressed: vec!["abandoned_finding".into()],
        });
        let rendered = render_findings_markdown(&result);
        let findings_at = rendered.find("passes even when").expect("finding shown");
        let fix_at = rendered.find("agent/remediation-x").expect("branch shown");
        assert!(
            findings_at < fix_at,
            "the finding must be read before the fix that assumes it:\n{rendered}"
        );
        assert!(
            rendered.contains("unverified"),
            "a fix for an unproven finding must say so:\n{rendered}"
        );
    }

    /// `CoderTuning::run_config` has silently dropped seven settings before now — the value
    /// parses, reaches nobody, and changing it does nothing. This is the check that costs a
    /// second and catches it.
    #[test]
    fn tuning_carries_session_critic_into_the_run_config() {
        let mut tuning = CoderTuning::default();
        tuning.session_critic.enabled = true;
        tuning.session_critic.remediation = true;
        tuning.session_critic.include_tool_names = false;
        let config = tuning.run_config();
        assert_eq!(
            config.session_critic, tuning.session_critic,
            "the setting parsed and then reached nobody"
        );
    }

    /// `prompt_dir` must survive the conversion, or `[coder] prompt_dir` becomes the ninth
    /// setting that parses and reaches nobody.
    #[test]
    fn tuning_carries_prompt_dir_into_the_run_config() {
        let tuning = CoderTuning {
            prompt_dir: Some("/etc/liberado/prompts".to_string()),
            ..CoderTuning::default()
        };
        assert_eq!(tuning.run_config().prompt_dir, tuning.prompt_dir);
    }

    /// Unconfigured must mean "the checkout the run is working in", not "the process's cwd".
    ///
    /// The first version of this resolved against cwd and this test caught it: `cargo test` runs
    /// with cwd at the crate directory, so the override silently fell back to the baked copy —
    /// and would have done the same inside every coding worktree, which is the one place a run
    /// most wants the checkout's own prompts.
    #[test]
    fn an_unconfigured_prompt_dir_resolves_inside_the_workspace() {
        assert!(CoderTuning::default().prompt_dir.is_none());
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("repo root")
            .to_string_lossy()
            .to_string();
        let dir = prompts::dir_for(None, &root);
        let from_disk = prompts::load(Some(&dir), prompts::CODER_FILE, "BAKED-FALLBACK");
        assert_ne!(
            from_disk, "BAKED-FALLBACK",
            "a run inside a checkout must read prompts/coder/coder.md from it, not the binary"
        );
    }

    #[test]
    fn a_configured_prompt_dir_is_used_verbatim() {
        assert_eq!(
            prompts::dir_for(Some("/etc/liberado/prompts"), "/some/workspace"),
            Path::new("/etc/liberado/prompts")
        );
    }

    /// The dangerous toggle must be off unless someone asks for it.
    #[test]
    fn remediation_is_off_by_default() {
        let config = SessionCriticConfig::default();
        assert!(!config.enabled, "the reviewer itself is opt-in");
        assert!(
            !config.remediation,
            "auto-fixing an unverified finding must never be the default"
        );
        assert!(
            config.include_tool_names,
            "dropping tool names cost two of four labelled traces; it must not be the default"
        );
    }
}

#[cfg(test)]
mod hashline_default_tests {
    use super::*;
    use std::path::Path;

    /// Hashline is **off** by default, and that is a measured position rather than a taste.
    ///
    /// #105 turned it on to end a divergence between the two coding paths. A four-run series on
    /// one task then measured it: with hashline on, `read_file` returns a line-numbered view
    /// while `edit_file` matches raw text, and the model pasted one into the other in 14 of 41
    /// calls — the worst anchor failure rate of the four (72%). The same task with hashline off
    /// scored best (42%) and produced 159 insertions with no deletions.
    ///
    /// The catalog is exclusive now, so hashline is no longer *broken*; it is simply not the
    /// default until a run measures it winning. Flipping this without that measurement should
    /// fail here.
    #[test]
    fn hashline_is_off_until_a_run_measures_it_winning() {
        let config = HashlineConfig::default();
        assert!(
            !config.enabled,
            "turning hashline on is a measured decision; the last measurement said off"
        );
        assert!(
            config.validate().is_ok(),
            "whatever the default is, it must satisfy its own validator: {:?}",
            config.validate()
        );
    }

    /// The warm-up is on by default, and the timeout must be longer than an honest cold build.
    ///
    /// A ceiling shorter than a real build turns "slow" into "the tree looks broken", which is
    /// the failure this replaced: a 120-second command timeout axed every workspace-wide cargo
    /// invocation and returned no output at all.
    #[test]
    fn the_warmup_is_on_and_its_ceiling_is_generous() {
        let config = WorkspaceBuildConfig::default();
        assert!(
            config.warmup,
            "a run should not discover a broken baseline from the model"
        );
        assert!(
            config.warmup_timeout_secs >= 600,
            "a cold build of this workspace is minutes; {}s would report a slow machine as a              broken tree",
            config.warmup_timeout_secs
        );
    }

    /// The command ceiling must clear a workspace build too, or the model's own checks die the
    /// way the warm-up used to.
    #[test]
    fn the_command_timeout_clears_a_workspace_build() {
        assert!(
            CommandPolicy::default().timeout_secs >= 600,
            "120s returned nothing from every cargo command a run tried"
        );
    }

    /// No shared cache by default. Cargo locks a target directory, so two concurrent runs queue
    /// rather than corrupt — measured — and a run queued behind a cold build times out having
    /// done nothing. Sharing is opt-in for the one-run-at-a-time case it is safe for.
    #[test]
    fn the_shared_cache_is_opt_in() {
        assert!(WorkspaceBuildConfig::default().shared_target_dir.is_none());
    }

    /// The number in `config.example/tuning.toml` must be the number the code uses.
    ///
    /// This replaced a test comparing `EditConfig::DEFAULT_FUZZY_THRESHOLD` against
    /// `EditConfig::default().fuzzy_threshold` — both read the same constant, so it passed
    /// whatever the constant was. A test that cannot fail is worse than no test; the drift that
    /// can actually happen is between the code and the file an operator reads.
    #[test]
    fn the_documented_threshold_matches_the_code() {
        let example = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(std::path::Path::parent)
                .expect("repo root")
                .join("config.example/tuning.toml"),
        )
        .expect("read config.example/tuning.toml");
        let documented = example
            .lines()
            .find_map(|l| l.trim().strip_prefix("# fuzzy_threshold = "))
            .expect("config.example must document fuzzy_threshold")
            .trim()
            .parse::<f64>()
            .expect("documented threshold must be a number");
        assert_eq!(
            documented,
            EditConfig::DEFAULT_FUZZY_THRESHOLD,
            "config.example says {documented} and the code uses {}",
            EditConfig::DEFAULT_FUZZY_THRESHOLD
        );
    }

    /// Fuzzy anchor matching is on by default, following `oh-my-pi`. Turning it off would
    /// reinstate the failure mode that accounted for a large share of four runs' rejected edits.
    #[test]
    fn fuzzy_anchor_matching_is_on_by_default() {
        let edit = EditConfig::default();
        assert!(
            edit.fuzzy_match,
            "exact-only matching was measured as worse"
        );
        assert!(
            (0.9..=1.0).contains(&edit.fuzzy_threshold),
            "a threshold outside 0.9..=1.0 either rejects everything or edits the wrong place: {}",
            edit.fuzzy_threshold
        );
    }

    /// A default that a run cannot use is not a default. `hash_length` outside
    /// `HASH_LENGTH_MIN..=HASH_LENGTH_MAX` fails `validate`, which would reject the config at
    /// load and leave the run with no edit tooling at all.
    #[test]
    fn the_default_hash_length_is_inside_its_own_bounds() {
        let length = HashlineConfig::default().hash_length;
        assert!(
            (HashlineConfig::HASH_LENGTH_MIN..=HashlineConfig::HASH_LENGTH_MAX).contains(&length),
            "default hash_length {length} is outside {}..={}",
            HashlineConfig::HASH_LENGTH_MIN,
            HashlineConfig::HASH_LENGTH_MAX
        );
    }

    /// Both coding paths must agree. The divergence is what caused this, not the value.
    #[test]
    fn tuning_is_the_single_source_for_hashline() {
        let tuning = CoderTuning::default();
        assert_eq!(
            tuning.run_config().hashline,
            tuning.hashline,
            "a path that hardcodes its own HashlineConfig can silently disagree with the other"
        );
    }
}
