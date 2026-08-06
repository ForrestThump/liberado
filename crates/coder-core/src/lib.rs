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
mod tuning;
mod verify;

pub use tuning::CoderTuning;

pub use coherence::{
    ContractFinding, Severity, contract_conflicts, contradictions, profile_injected_ids,
};
pub use intake::{
    FreezeAuthority, GoalContract, GoalContractDraft, IntakeOutcome, IntakeQuestion,
    expand_verify_profile_into, intake_outcome_schema, profile_verifiers, sanitize_draft,
    validate_draft,
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
    /// No shell programs may run (plan / explore presets).
    ///
    /// Reuses the existing allow-list rule in `coder-sandbox`: a **non-empty** `allow` list that
    /// matches nothing denies every command. Empty `allow` would mean "allow all".
    pub fn none_allowed() -> Self {
        Self {
            allow: vec!["!mode-no-shell".into()],
            deny: Vec::new(),
            timeout_secs: 120,
            output_max_bytes: 64 * 1024,
        }
    }
}

/// System instructions injected when a coding session runs in plan mode.
///
/// Kept next to the policy helpers so pack and surfaces do not each invent plan-mode prose.
pub const PLAN_MODE_CODER_PROMPT: &str = "\
You are Liberado's coding planner (plan mode). Explore the codebase with read-only tools, then \
write a clear implementation plan ONLY to `.liberado/plan.md`. \
Do NOT edit any other files. Do NOT run shell commands, git commits, or apply patches outside that path. \
When the plan is written, call submit_report summarizing the plan and key risks.";

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
            read_only_turn_limit: 4,
            same_tool_limit: 3,
            validation_repeat_limit: 2,
            max_attempts: 3,
            event_preview_max_chars: 500,
        }
    }
}

/// Fully resolved settings for one backend run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoderRunConfig {
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_dir: Option<String>,
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
}
