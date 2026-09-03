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
mod failure_excerpt;
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
pub use failure_excerpt::{
    EXTRACT_MAX_LINES as FAILURE_EXTRACT_MAX_LINES, extract_failures, extract_failures_capped,
    log_tail,
};
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

/// Optional write restriction attached to one coding dispatch.
///
/// A non-empty allow list is a complete allowlist and takes precedence over the deny list. With
/// no allow list, the deny list blocks matching paths. This scope can narrow a dispatch, but it
/// never relaxes the enclosing [`PathPolicy`] or a restricted [`CodingMode`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchWriteScope {
    #[serde(default)]
    pub allow_globs: Vec<String>,
    #[serde(default)]
    pub deny_globs: Vec<String>,
}

impl DispatchWriteScope {
    /// Whether this scope changes the default per-dispatch write behavior.
    pub fn is_active(&self) -> bool {
        !self.allow_globs.is_empty() || !self.deny_globs.is_empty()
    }

    /// Whether this dispatch scope permits one workspace-relative path.
    ///
    /// The match syntax deliberately mirrors the coding tool path policy: an exact path, a
    /// directory prefix ending in `/**`, or `**` for the whole workspace.
    pub fn permits(&self, relative_path: &str) -> bool {
        if self.allow_globs.is_empty() {
            !self
                .deny_globs
                .iter()
                .any(|pattern| scope_path_matches(pattern, relative_path))
        } else {
            self.allow_globs
                .iter()
                .any(|pattern| scope_path_matches(pattern, relative_path))
        }
    }
}

fn scope_path_matches(pattern: &str, relative_path: &str) -> bool {
    let pattern = pattern.replace('\\', "/");
    let relative_path = relative_path.replace('\\', "/");
    pattern == "**"
        || pattern == relative_path
        || pattern.strip_suffix("/**").is_some_and(|prefix| {
            relative_path == prefix || relative_path.starts_with(&format!("{prefix}/"))
        })
}

/// Path containment and write policy for the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PathPolicy {
    #[serde(default = "default_allow_write_globs")]
    pub allow_write_globs: Vec<String>,
    #[serde(default = "default_deny_globs")]
    pub deny_globs: Vec<String>,
    /// Optional restriction supplied by the dispatcher for this one task. It is intentionally
    /// separate from the persistent policy: the latter remains the hard capability ceiling.
    #[serde(default)]
    pub write_scope: DispatchWriteScope,
    #[serde(default = "default_read_max_bytes")]
    pub read_max_bytes: usize,
    #[serde(default = "default_search_max_results")]
    pub search_max_results: usize,
}

impl Default for PathPolicy {
    fn default() -> Self {
        Self {
            allow_write_globs: default_allow_write_globs(),
            deny_globs: default_deny_globs(),
            write_scope: DispatchWriteScope::default(),
            read_max_bytes: default_read_max_bytes(),
            search_max_results: default_search_max_results(),
        }
    }
}

fn default_allow_write_globs() -> Vec<String> {
    vec!["**".to_string()]
}

fn default_deny_globs() -> Vec<String> {
    vec![
        ".git/**".to_string(),
        "target/**".to_string(),
        "node_modules/**".to_string(),
    ]
}

fn default_read_max_bytes() -> usize {
    128 * 1024
}

fn default_search_max_results() -> usize {
    200
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
    /// An optional `CARGO_TARGET_DIR` for worktrees from one controlled source root.
    ///
    /// A worktree starts with no build cache, so the run's first compile rebuilds the whole
    /// dependency graph — 706 of this workspace's 770 packages are registry crates, and those are
    /// keyed by registry/name/version rather than by path, so worktrees from that source can reuse
    /// them. Distinct source roots must use distinct target directories. A live comparison reused
    /// a passing test binary built from another checkout after the active source had changed.
    ///
    /// **One source root and one run at a time.** Cargo takes an exclusive lock on a target
    /// directory; concurrent builds do not corrupt each other, they queue. Measured: a second
    /// `cargo build` printed
    /// "Blocking waiting for file lock on artifact directory" and waited out the first. With a
    /// command timeout in play, a run queued behind a cold build times out having done nothing,
    /// which is worse than giving it its own cache. Sharing safely across concurrent runs needs
    /// a lock-free compiler cache such as `sccache`, not this.
    ///
    /// Unset means each worktree keeps its own `target/` unless
    /// [`Self::managed_target_root`] is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_target_dir: Option<String>,
    /// Optional pool root for class-aware shared and isolated Cargo targets.
    ///
    /// Ordinary jobs from one source root share `<root>/shared/<source>/ordinary`.
    /// Coverage, mutation, and comparison jobs use `<root>/isolated/<class>/<job>`.
    /// When [`Self::shared_target_dir`] is also set, that exact path still wins for
    /// ordinary coding (C3 pins and existing operators). Unset keeps worktree-local
    /// `target/` — this field does not change coding-pack defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_target_root: Option<String>,
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
            managed_target_root: None,
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
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lib_findings_tests.rs"]
mod findings_tests;

#[cfg(test)]
#[path = "lib_hashline_default_tests.rs"]
mod hashline_default_tests;

#[cfg(test)]
#[path = "lib_mode_scope_survivor_tests.rs"]
mod mode_scope_survivor_tests;
