//! One production coding-run assembly path.
//!
//! `CodingSessionPack`, ACP, and the headless runner used to each hand-build a
//! `CoderRunConfig`. Shared knobs then drifted: gate enablement, progress limits, traces,
//! verifiers. This module is the single place that turns [`CoderTuning`] plus surface-only inputs
//! into a resolved [`CoderRunRequest`], and records **where each critical field came from**.
//!
//! Pure resolution: no provider, no worktree materialization, no network. Surfaces still own
//! sandbox setup and I/O; they call this only for config field resolution.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use liberado_coder_core::{
    CoderRoleConfig, CoderRunConfig, CoderRunRequest, CoderTask, CoderTuning, CodingMode,
    CommandPolicy, HashlineConfig, PathPolicy, ProgressPolicy, SandboxSpec, TraceFormat,
    WorkspaceRef, default_verifiers,
};
use serde::{Deserialize, Serialize};

/// How the critic role is filled for this surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticPolicy {
    /// Keep `tuning.critic` as-is.
    FromTuning,
    /// Standalone critic loop off (no prompt, `max_turns = 0`), but keep
    /// `[coder.critic].model` so gate and session-critic fallbacks do not use the coder model.
    Disabled,
    /// Load the cold-diff-reviewer prompt (ACP). Gate reviewers need a real prompt, not an empty
    /// disabled role — empty prompts error when the gate is enabled.
    ReviewerWithLoadedPrompt,
}

/// How the repair role is filled when the mode is not restricted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairPolicy {
    /// `tuning.repair` as-is (with model / max_turns surface overrides when set).
    FromTuning,
    /// `Some(coder_role)` — pack normal-mode behaviour.
    MirrorCoder,
    /// No repair loop.
    None,
}

/// What to do when `tuning.verifiers` is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyVerifiersPolicy {
    /// Leave empty (session pack: the frozen contract fills them later).
    LeaveEmpty,
    /// Fill with [`default_verifiers`] for the workspace (ACP / headless).
    DefaultForWorkspace,
}

/// How to resolve `trace_dir` from the configured value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceDirPolicy {
    /// Use `tuning.trace_dir` as written (relative stays relative).
    AsConfigured,
    /// Join relative paths to `workspace_path`; absolute paths stay absolute.
    RelativeToWorkspace,
    /// If tuning has no dir, fall back to `$LIBERADO_DATA_DIR/coder-traces` (else `.liberado/…`).
    DataDirFallback,
}

/// Surface-only inputs that may legitimately differ per entry point.
///
/// Shared tuning fields (`gate`, `progress`, `hashline`, `edit`, …) come from [`CoderTuning`].
/// Do not re-hardcode those here.
#[derive(Debug, Clone)]
pub struct ProductionSurface {
    pub task: CoderTask,
    pub workspace: WorkspaceRef,
    /// Filesystem root of the run (default verifiers, relative traces, prompt dir resolution).
    pub workspace_path: PathBuf,
    pub sandbox: SandboxSpec,
    pub attempt: u32,
    pub prior_feedback: Vec<String>,
    pub strategist_directive: Option<String>,
    /// When set, fully replaces the coder role (pack builds mode-specific prompts).
    pub coder_role: Option<CoderRoleConfig>,
    /// Override `coder.model` (and repair when present) when `coder_role` is `None`.
    pub model_override: Option<String>,
    /// Override `coder.max_turns` (and repair) when `coder_role` is `None`.
    pub max_turns: Option<u32>,
    pub mode: CodingMode,
    /// When set, replaces `tuning.command_policy` (pack mode policies).
    pub command_policy: Option<CommandPolicy>,
    /// When set, replaces `tuning.path_policy`.
    pub path_policy: Option<PathPolicy>,
    /// When set, replaces `tuning.hashline` (pack may force hashline off in explore).
    pub hashline: Option<HashlineConfig>,
    pub critic: CriticPolicy,
    pub repair: RepairPolicy,
    pub empty_verifiers: EmptyVerifiersPolicy,
    pub trace_dir: TraceDirPolicy,
    /// ACP: empty `allow_write_globs` on tuning means "use PathPolicy::default()", not a locked tree.
    pub default_empty_path_policy: bool,
    /// Production direct-execution paths disable the planner today.
    pub disable_planner: bool,
}

impl Default for ProductionSurface {
    fn default() -> Self {
        Self {
            task: CoderTask::new("task", ""),
            workspace: WorkspaceRef::new(".", "HEAD"),
            workspace_path: PathBuf::from("."),
            sandbox: SandboxSpec::HostLocal,
            attempt: 0,
            prior_feedback: Vec::new(),
            strategist_directive: None,
            coder_role: None,
            model_override: None,
            max_turns: None,
            mode: CodingMode::Normal,
            command_policy: None,
            path_policy: None,
            hashline: None,
            critic: CriticPolicy::FromTuning,
            repair: RepairPolicy::FromTuning,
            empty_verifiers: EmptyVerifiersPolicy::LeaveEmpty,
            trace_dir: TraceDirPolicy::AsConfigured,
            default_empty_path_policy: false,
            disable_planner: true,
        }
    }
}

/// Source label for one resolved field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSource {
    pub field: String,
    /// e.g. `tuning`, `surface.model_override`, `default_verifiers`, `mode.explore`.
    pub source: String,
}

/// Provenance map for a resolved production run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssemblyProvenance {
    pub fields: BTreeMap<String, String>,
}

impl AssemblyProvenance {
    pub fn record(&mut self, field: impl Into<String>, source: impl Into<String>) {
        self.fields.insert(field.into(), source.into());
    }

    pub fn source_of(&self, field: &str) -> Option<&str> {
        self.fields.get(field).map(String::as_str)
    }

    pub fn entries(&self) -> impl Iterator<Item = FieldSource> + '_ {
        self.fields.iter().map(|(field, source)| FieldSource {
            field: field.clone(),
            source: source.clone(),
        })
    }
}

/// Fully resolved production request plus provenance of critical fields.
#[derive(Debug, Clone)]
pub struct AssembledRun {
    pub request: CoderRunRequest,
    pub provenance: AssemblyProvenance,
}

/// Resolve a production coding run from shared tuning + surface-only inputs.
///
/// Starts from [`CoderTuning::run_config`] so every shared field has a single passthrough path,
/// then applies deliberate surface policy. Critical fields are recorded in
/// [`AssembledRun::provenance`].
/// Mutable assembly state: the config being resolved plus the provenance ledger.
struct AssemblyState {
    config: CoderRunConfig,
    provenance: AssemblyProvenance,
}

/// Baseline: every field that `run_config` copies is from tuning until a surface overrides it.
fn apply_baseline(sandbox: SandboxSpec, assembly: &mut AssemblyState) {
    for field in BASELINE_TUNING_FIELDS {
        assembly.provenance.record(*field, "tuning");
    }
    assembly.provenance.record("sandbox", "surface.sandbox");
    assembly.config.sandbox = sandbox;
}

/// Coder role: a surface role fully replaces tuning; otherwise the model/max_turns overrides apply.
fn apply_coder_role(
    coder_role: Option<CoderRoleConfig>,
    model_override: Option<&str>,
    max_turns: Option<u32>,
    assembly: &mut AssemblyState,
) {
    if let Some(role) = coder_role {
        assembly.config.coder = role;
        assembly.provenance.record("coder", "surface.coder_role");
        assembly
            .provenance
            .record("coder.model", "surface.coder_role");
        assembly
            .provenance
            .record("coder.max_turns", "surface.coder_role");
    } else {
        if let Some(model) = model_override
            && !model.is_empty()
        {
            assembly.config.coder.model = model.to_string();
            assembly
                .provenance
                .record("coder.model", "surface.model_override");
        }
        if let Some(max_turns) = max_turns {
            assembly.config.coder.max_turns = Some(max_turns);
            assembly
                .provenance
                .record("coder.max_turns", "surface.max_turns");
        }
    }
}

/// Planner: production direct-execution paths disable it today.
fn apply_planner(disable_planner: bool, model_for_disabled: &str, assembly: &mut AssemblyState) {
    if disable_planner {
        assembly.config.planner = disabled_role(model_for_disabled);
        assembly.provenance.record("planner", "surface.disabled");
    }
}

/// Critic: from tuning, disabled (coder-model fallback when tuning has no critic model), or a
/// loaded diff-reviewer prompt resolved against the prompt dir.
fn apply_critic(
    tuning: &CoderTuning,
    critic: CriticPolicy,
    workspace_path: &Path,
    model_for_disabled: &str,
    assembly: &mut AssemblyState,
) {
    match critic {
        CriticPolicy::FromTuning => {}
        CriticPolicy::Disabled => {
            let model = if tuning.critic.model.trim().is_empty() {
                model_for_disabled
            } else {
                tuning.critic.model.as_str()
            };
            assembly.config.critic = disabled_role(model);
            assembly.provenance.record("critic", "surface.disabled");
            assembly.provenance.record(
                "critic.model",
                if tuning.critic.model.trim().is_empty() {
                    "surface.coder_fallback"
                } else {
                    "tuning"
                },
            );
        }
        CriticPolicy::ReviewerWithLoadedPrompt => {
            assembly.config.critic = reviewer_role(
                &tuning.critic,
                model_for_disabled,
                Some(&liberado_coder_core::prompts::dir_for(
                    tuning.prompt_dir.as_deref(),
                    &workspace_path.to_string_lossy(),
                )),
            );
            assembly
                .provenance
                .record("critic", "surface.reviewer_prompt");
            assembly.provenance.record("critic.model", "tuning");
        }
    }
}

/// Repair: restricted mode drops it; otherwise surface policy replaces tuning or mirrors coder.
fn apply_repair(
    mode: CodingMode,
    repair: RepairPolicy,
    model_override: Option<&str>,
    max_turns: Option<u32>,
    assembly: &mut AssemblyState,
) {
    if mode.is_restricted() {
        assembly.config.repair = None;
        assembly.provenance.record("repair", "mode.restricted");
    } else {
        match repair {
            RepairPolicy::FromTuning => {
                if let Some(mut repair) = assembly.config.repair.take() {
                    if let Some(model) = model_override
                        && !model.is_empty()
                    {
                        repair.model = model.to_string();
                        assembly
                            .provenance
                            .record("repair.model", "surface.model_override");
                    }
                    if let Some(max_turns) = max_turns {
                        repair.max_turns = Some(max_turns);
                        assembly
                            .provenance
                            .record("repair.max_turns", "surface.max_turns");
                    }
                    assembly.config.repair = Some(repair);
                } else {
                    assembly.provenance.record("repair", "tuning.none");
                }
            }
            RepairPolicy::MirrorCoder => {
                assembly.config.repair = Some(assembly.config.coder.clone());
                assembly.provenance.record("repair", "surface.mirror_coder");
            }
            RepairPolicy::None => {
                assembly.config.repair = None;
                assembly.provenance.record("repair", "surface.none");
            }
        }
    }
}

/// Command / path policy. Gate stays untouched: always from tuning — never hardcode
/// enabled: false at a surface (provenance already records "gate" → "tuning").
fn apply_command_and_path_policy(
    command_policy: Option<CommandPolicy>,
    path_policy: Option<PathPolicy>,
    default_empty_path_policy: bool,
    assembly: &mut AssemblyState,
) {
    if let Some(policy) = command_policy {
        assembly.config.command_policy = policy;
        assembly
            .provenance
            .record("command_policy", "surface.command_policy");
    }
    if let Some(policy) = path_policy {
        assembly.config.path_policy = policy;
        assembly
            .provenance
            .record("path_policy", "surface.path_policy");
    } else if default_empty_path_policy && assembly.config.path_policy.allow_write_globs.is_empty()
    {
        assembly.config.path_policy = PathPolicy::default();
        assembly
            .provenance
            .record("path_policy", "surface.default_empty_path_policy");
    }
}

fn apply_hashline(hashline: Option<HashlineConfig>, assembly: &mut AssemblyState) {
    if let Some(hashline) = hashline {
        assembly.config.hashline = hashline;
        assembly.provenance.record("hashline", "surface.hashline");
    }
}

/// Verifiers: tuning's set is kept as-is; an empty set resolves by policy (or stays empty).
fn apply_verifiers(
    empty_verifiers: EmptyVerifiersPolicy,
    workspace_path: &Path,
    assembly: &mut AssemblyState,
) {
    if assembly.config.verifiers.is_empty() {
        match empty_verifiers {
            EmptyVerifiersPolicy::LeaveEmpty => {
                assembly
                    .provenance
                    .record("verifiers", "surface.leave_empty");
            }
            EmptyVerifiersPolicy::DefaultForWorkspace => {
                assembly.config.verifiers = default_verifiers(workspace_path);
                assembly.provenance.record("verifiers", "default_verifiers");
            }
        }
    } else {
        assembly.provenance.record("verifiers", "tuning");
    }
}

/// Progress: mode may widen/narrow limits (restricted pins max_attempts, explore lifts the
/// read-only and same-tool guards).
fn apply_progress(tuning: &CoderTuning, mode: CodingMode, assembly: &mut AssemblyState) {
    assembly.config.progress = resolve_progress(&tuning.progress, mode, &mut assembly.provenance);
}

/// Trace dir / formats: surface policy resolves the directory; an empty formats list defaults
/// to native traces.
fn apply_trace(trace_dir: TraceDirPolicy, workspace_path: &Path, assembly: &mut AssemblyState) {
    assembly.config.trace_dir = resolve_trace_dir(
        assembly.config.trace_dir.take(),
        &trace_dir,
        workspace_path,
        &mut assembly.provenance,
    );
    if assembly.config.trace_formats.is_empty() {
        assembly.config.trace_formats = vec![TraceFormat::Native];
        assembly
            .provenance
            .record("trace_formats", "default.native");
    }
}

/// Resolve a production coding run from shared tuning + surface-only inputs.
///
/// Starts from [`CoderTuning::run_config`] so every shared field has a single passthrough path,
/// then applies deliberate surface policy. Critical fields are recorded in
/// [`AssembledRun::provenance`].
pub fn assemble_production_run(tuning: &CoderTuning, surface: ProductionSurface) -> AssembledRun {
    let ProductionSurface {
        task,
        workspace,
        workspace_path,
        sandbox,
        attempt,
        prior_feedback,
        strategist_directive,
        coder_role,
        model_override,
        max_turns,
        mode,
        command_policy,
        path_policy,
        hashline,
        critic,
        repair,
        empty_verifiers,
        trace_dir,
        default_empty_path_policy,
        disable_planner,
    } = surface;

    let mut assembly = AssemblyState {
        config: tuning.run_config(),
        provenance: AssemblyProvenance::default(),
    };

    apply_baseline(sandbox, &mut assembly);
    apply_coder_role(
        coder_role,
        model_override.as_deref(),
        max_turns,
        &mut assembly,
    );
    let model_for_disabled = assembly.config.coder.model.clone();
    apply_planner(disable_planner, &model_for_disabled, &mut assembly);
    apply_critic(
        tuning,
        critic,
        &workspace_path,
        &model_for_disabled,
        &mut assembly,
    );
    apply_repair(
        mode,
        repair,
        model_override.as_deref(),
        max_turns,
        &mut assembly,
    );
    apply_command_and_path_policy(
        command_policy,
        path_policy,
        default_empty_path_policy,
        &mut assembly,
    );
    apply_hashline(hashline, &mut assembly);
    apply_verifiers(empty_verifiers, &workspace_path, &mut assembly);
    apply_progress(tuning, mode, &mut assembly);
    apply_trace(trace_dir, &workspace_path, &mut assembly);

    let request = CoderRunRequest {
        task,
        workspace,
        config: assembly.config,
        attempt,
        prior_feedback,
        strategist_directive,
    };

    AssembledRun {
        request,
        provenance: assembly.provenance,
    }
}
/// Fields that `CoderTuning::run_config` copies 1:1. Used to seed provenance.
const BASELINE_TUNING_FIELDS: &[&str] = &[
    "backend",
    "trace_dir",
    "trace_formats",
    "planner",
    "coder",
    "coder.model",
    "coder.max_turns",
    "critic",
    "gate",
    "repair",
    "command_policy",
    "validation_command",
    "verifiers",
    "verify_policy",
    "path_policy",
    "progress",
    "hashline",
    "session_critic",
    "prompt_dir",
    "edit",
    "workspace_build",
];

fn resolve_progress(
    base: &ProgressPolicy,
    mode: CodingMode,
    provenance: &mut AssemblyProvenance,
) -> ProgressPolicy {
    let mut progress = base.clone();
    if mode.is_restricted() {
        progress.max_attempts = 1;
        provenance.record("progress.max_attempts", "mode.restricted");
    }
    if mode == CodingMode::Explore {
        // Explore is exclusively read-only tools — the progress guard's read_only_turn_limit and
        // same_tool_limit would fire false stalls because every explore tool is non-mutating.
        progress.read_only_turn_limit = u32::MAX;
        progress.same_tool_limit = u32::MAX;
        provenance.record("progress.read_only_turn_limit", "mode.explore");
        provenance.record("progress.same_tool_limit", "mode.explore");
    }
    progress
}

fn resolve_trace_dir(
    configured: Option<String>,
    policy: &TraceDirPolicy,
    workspace_path: &Path,
    provenance: &mut AssemblyProvenance,
) -> Option<String> {
    match policy {
        TraceDirPolicy::AsConfigured => {
            provenance.record("trace_dir", "tuning");
            configured
        }
        TraceDirPolicy::RelativeToWorkspace => {
            let raw = configured.unwrap_or_else(|| "coder-traces".into());
            let path = Path::new(&raw);
            let resolved = if path.is_absolute() {
                raw
            } else {
                workspace_path.join(path).to_string_lossy().into_owned()
            };
            provenance.record("trace_dir", "surface.relative_to_workspace");
            Some(resolved)
        }
        TraceDirPolicy::DataDirFallback => {
            let from_tuning = configured.is_some();
            let resolved = configured.or_else(|| {
                Some(
                    PathBuf::from(
                        std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into()),
                    )
                    .join("coder-traces")
                    .to_string_lossy()
                    .into_owned(),
                )
            });
            provenance.record(
                "trace_dir",
                if from_tuning {
                    "tuning"
                } else {
                    "surface.data_dir_fallback"
                },
            );
            resolved
        }
    }
}

fn disabled_role(model: &str) -> CoderRoleConfig {
    CoderRoleConfig {
        model: model.to_string(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: Some(0),
        reasoning: None,
    }
}

/// The cold diff reviewer the completion gate consults (shared with ACP's prior local copy).
fn reviewer_role(
    configured: &CoderRoleConfig,
    fallback_model: &str,
    prompt_dir: Option<&Path>,
) -> CoderRoleConfig {
    let model = if configured.model.trim().is_empty() {
        fallback_model.to_string()
    } else {
        configured.model.clone()
    };
    CoderRoleConfig {
        model,
        prompt_path: None,
        prompt: Some(liberado_coder_core::prompts::load(
            prompt_dir,
            liberado_coder_core::prompts::DIFF_REVIEWER_FILE,
            liberado_coder_core::prompts::DIFF_REVIEWER,
        )),
        temperature: configured.temperature.or(Some(0.0)),
        max_tokens: configured.max_tokens.or(Some(4000)),
        max_turns: Some(1),
        reasoning: None,
    }
}

/// Thin wrappers the three production entry points expose for conformance tests.
///
/// Each builds the same surface shape its production caller uses, so a fixture that drives these
/// exercises the real assembly path — not a reimplementation.
pub mod entry {
    use super::*;

    /// Inputs that are unique to the session-pack build path.
    pub struct PackSurfaceArgs {
        pub task: CoderTask,
        pub workspace_path: PathBuf,
        pub sandbox: SandboxSpec,
        pub coder_role: CoderRoleConfig,
        pub mode: CodingMode,
        pub command_policy: CommandPolicy,
        pub path_policy: PathPolicy,
        pub hashline: HashlineConfig,
    }

    /// Session-pack build assembly: mode policies, contract fills verifiers later, critic off,
    /// repair mirrors coder when not restricted.
    pub fn pack_surface(args: PackSurfaceArgs) -> ProductionSurface {
        ProductionSurface {
            task: args.task,
            workspace: WorkspaceRef::new(args.workspace_path.to_string_lossy(), "HEAD"),
            workspace_path: args.workspace_path,
            sandbox: args.sandbox,
            attempt: 0,
            prior_feedback: Vec::new(),
            strategist_directive: None,
            coder_role: Some(args.coder_role),
            model_override: None,
            max_turns: None,
            mode: args.mode,
            command_policy: Some(args.command_policy),
            path_policy: Some(args.path_policy),
            hashline: Some(args.hashline),
            critic: CriticPolicy::Disabled,
            repair: RepairPolicy::MirrorCoder,
            empty_verifiers: EmptyVerifiersPolicy::LeaveEmpty,
            trace_dir: TraceDirPolicy::AsConfigured,
            default_empty_path_policy: false,
            disable_planner: true,
        }
    }

    /// ACP coding dispatch: reviewer critic, default verifiers, data-dir trace fallback.
    pub fn acp_surface(
        task: CoderTask,
        workspace_path: PathBuf,
        model_override: Option<String>,
        max_turns: Option<u32>,
        attempt: u32,
        prior_feedback: Vec<String>,
    ) -> ProductionSurface {
        ProductionSurface {
            task,
            workspace: WorkspaceRef::new(workspace_path.to_string_lossy(), "HEAD"),
            workspace_path,
            sandbox: SandboxSpec::HostLocal,
            attempt,
            prior_feedback,
            strategist_directive: None,
            coder_role: None,
            model_override,
            max_turns,
            mode: CodingMode::Normal,
            command_policy: None,
            path_policy: None,
            hashline: None,
            critic: CriticPolicy::ReviewerWithLoadedPrompt,
            repair: RepairPolicy::FromTuning,
            empty_verifiers: EmptyVerifiersPolicy::DefaultForWorkspace,
            trace_dir: TraceDirPolicy::DataDirFallback,
            default_empty_path_policy: true,
            disable_planner: true,
        }
    }

    /// Headless `liberado-coder-run task run`: no repair, default verifiers, workspace-relative
    /// traces. Gate comes from tuning (not a hardcoded `enabled: false`).
    pub fn runner_surface(
        task: CoderTask,
        workspace_path: PathBuf,
        model: Option<String>,
        max_turns: Option<u32>,
    ) -> ProductionSurface {
        ProductionSurface {
            task,
            workspace: WorkspaceRef::new(workspace_path.to_string_lossy(), "HEAD"),
            workspace_path,
            sandbox: SandboxSpec::HostLocal,
            attempt: 0,
            prior_feedback: Vec::new(),
            strategist_directive: None,
            coder_role: None,
            model_override: model,
            max_turns,
            mode: CodingMode::Normal,
            command_policy: None,
            path_policy: None,
            hashline: None,
            critic: CriticPolicy::Disabled,
            repair: RepairPolicy::None,
            empty_verifiers: EmptyVerifiersPolicy::DefaultForWorkspace,
            trace_dir: TraceDirPolicy::RelativeToWorkspace,
            default_empty_path_policy: false,
            disable_planner: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::CoderGateConfig;

    fn twisted_tuning() -> CoderTuning {
        CoderTuning {
            gate: CoderGateConfig {
                enabled: true,
                fresh_reviewers: 2,
                ..Default::default()
            },
            progress: ProgressPolicy {
                read_only_turn_limit: 17,
                same_tool_limit: 13,
                max_attempts: 5,
                ..Default::default()
            },
            trace_dir: Some("custom-traces".into()),
            trace_formats: vec![TraceFormat::Native, TraceFormat::OpenaiMessages],
            hashline: HashlineConfig {
                enabled: true,
                hash_length: 8,
            },
            edit: liberado_coder_core::EditConfig {
                fuzzy_match: false,
                ..Default::default()
            },
            workspace_build: liberado_coder_core::WorkspaceBuildConfig {
                warmup: false,
                warmup_timeout_secs: 99,
                ..Default::default()
            },
            prompt_dir: Some("/prompts".into()),
            session_critic: liberado_coder_core::SessionCriticConfig {
                enabled: true,
                ..Default::default()
            },
            coder: CoderRoleConfig {
                model: "fixture-coder".into(),
                max_turns: Some(42),
                ..CoderTuning::default().coder
            },
            critic: CoderRoleConfig {
                model: "fixture-critic".into(),
                ..CoderTuning::default().critic
            },
            command_policy: CommandPolicy {
                timeout_secs: 123,
                ..Default::default()
            },
            path_policy: PathPolicy {
                read_max_bytes: 456,
                ..Default::default()
            },
            ..CoderTuning::default()
        }
    }

    /// One fixture drives all three entry-path tests, so the fields they check are compared on
    /// identical inputs. Touching `Cargo.toml` in the workspace lets `default_verifiers` include
    /// cargo checks when used.
    fn fixture_runs() -> (
        CoderTuning,
        CoderTask,
        AssembledRun,
        AssembledRun,
        AssembledRun,
    ) {
        let tuning = twisted_tuning();
        let ws = std::env::temp_dir().join("liberado-assemble-fixture");
        let _ = std::fs::create_dir_all(&ws);
        let _ = std::fs::write(
            ws.join("Cargo.toml"),
            "[package]\nname=\"t\"\nversion=\"0.0.0\"\n",
        );

        let task = CoderTask::new("t1", "do the thing");
        let pack = assemble_production_run(
            &tuning,
            entry::pack_surface(entry::PackSurfaceArgs {
                task: task.clone(),
                workspace_path: ws.clone(),
                sandbox: SandboxSpec::HostLocal,
                coder_role: CoderRoleConfig {
                    model: "pack-model".into(),
                    prompt_path: None,
                    prompt: Some("pack prompt".into()),
                    temperature: Some(0.1),
                    max_tokens: None,
                    max_turns: Some(12),
                    reasoning: None,
                },
                mode: CodingMode::Normal,
                command_policy: tuning.command_policy.clone(),
                path_policy: tuning.path_policy.clone(),
                hashline: tuning.hashline.clone(),
            }),
        );
        let acp = assemble_production_run(
            &tuning,
            entry::acp_surface(
                task.clone(),
                ws.clone(),
                Some("acp-model".into()),
                Some(50),
                0,
                Vec::new(),
            ),
        );
        let runner = assemble_production_run(
            &tuning,
            entry::runner_surface(task.clone(), ws, Some("runner-model".into()), Some(50)),
        );
        (tuning, task, pack, acp, runner)
    }

    #[test]
    fn three_entry_paths_share_tuning_fields_from_one_fixture() {
        let (tuning, _task, pack, acp, runner) = fixture_runs();

        // Shared tuning fields that historically drifted must match across all three.
        for (name, assembled) in [("pack", &pack), ("acp", &acp), ("runner", &runner)] {
            let c = &assembled.request.config;
            assert!(
                c.gate.enabled,
                "{name}: gate.enabled must come from tuning, not a surface hardcode"
            );
            assert_eq!(c.gate.fresh_reviewers, 2, "{name}: gate.fresh_reviewers");
            assert_eq!(
                assembled.provenance.source_of("gate"),
                Some("tuning"),
                "{name}: gate provenance"
            );
            assert!(!c.edit.fuzzy_match, "{name}: edit from tuning");
            assert_eq!(
                c.workspace_build.warmup_timeout_secs, 99,
                "{name}: workspace_build from tuning"
            );
            assert_eq!(
                c.prompt_dir.as_deref(),
                Some("/prompts"),
                "{name}: prompt_dir from tuning"
            );
            assert!(
                c.session_critic.enabled,
                "{name}: session_critic from tuning"
            );
            assert_eq!(
                c.hashline.hash_length, 8,
                "{name}: hashline from tuning (pack surface passed same value)"
            );
            assert_eq!(
                c.verify_policy, tuning.verify_policy,
                "{name}: verify_policy from tuning"
            );
            assert_eq!(
                assembled.provenance.source_of("progress"),
                Some("tuning"),
                "{name}: progress baseline provenance"
            );
            // Normal mode keeps tuning progress limits (explore would widen them).
            assert_eq!(c.progress.read_only_turn_limit, 17, "{name}: progress");
            assert_eq!(c.progress.same_tool_limit, 13, "{name}: progress");
            assert_eq!(c.progress.max_attempts, 5, "{name}: progress max_attempts");
        }
    }

    #[test]
    fn entry_paths_keep_surface_specific_overrides() {
        let (_tuning, _task, pack, acp, runner) = fixture_runs();

        // Intentional deltas — documented, not silent hardcodes.
        assert_eq!(pack.request.task.description, "do the thing");
        assert_eq!(acp.request.task.description, "do the thing");
        assert_eq!(runner.request.task.description, "do the thing");
        assert_eq!(pack.request.config.coder.model, "pack-model");
        assert_eq!(
            pack.provenance.source_of("coder.model"),
            Some("surface.coder_role")
        );
        assert_eq!(acp.request.config.coder.model, "acp-model");
        assert_eq!(
            acp.provenance.source_of("coder.model"),
            Some("surface.model_override")
        );
        assert_eq!(runner.request.config.coder.model, "runner-model");
        assert_eq!(acp.request.config.coder.max_turns, Some(50));
        assert_eq!(runner.request.config.coder.max_turns, Some(50));

        // Pack policies that must not fall through to ProductionSurface::default().
        assert_eq!(
            pack.request.config.command_policy.timeout_secs, 123,
            "pack command_policy must be the surface override"
        );
        assert_eq!(
            pack.request.config.path_policy.read_max_bytes, 456,
            "pack path_policy must be the surface override"
        );
        assert_eq!(
            pack.provenance.source_of("command_policy"),
            Some("surface.command_policy")
        );
    }

    #[test]
    fn verifiers_and_critic_policies_differ_by_entry_path() {
        let (_tuning, _task, pack, acp, runner) = fixture_runs();

        assert!(
            pack.request.config.verifiers.is_empty(),
            "pack leaves verifiers empty for the contract"
        );
        assert_eq!(
            pack.provenance.source_of("verifiers"),
            Some("surface.leave_empty")
        );
        assert!(
            !acp.request.config.verifiers.is_empty(),
            "acp fills default verifiers"
        );
        assert_eq!(
            acp.provenance.source_of("verifiers"),
            Some("default_verifiers")
        );
        assert!(
            !runner.request.config.verifiers.is_empty(),
            "runner fills default verifiers"
        );

        // Critic policies — disabled means *no* prompt source (inline or path). Checking only
        // `.prompt` would miss a FromTuning default that still carries prompt_path.
        assert!(
            pack.request.config.critic.prompt.is_none()
                && pack.request.config.critic.prompt_path.is_none(),
            "pack critic disabled"
        );
        assert!(
            acp.request.config.critic.prompt.is_some(),
            "acp critic has loaded reviewer prompt"
        );
        assert_eq!(
            acp.provenance.source_of("critic"),
            Some("surface.reviewer_prompt")
        );
        assert!(
            runner.request.config.critic.prompt.is_none()
                && runner.request.config.critic.prompt_path.is_none(),
            "runner critic disabled"
        );
        assert_eq!(
            pack.request.config.critic.model, "fixture-critic",
            "pack Disabled critic keeps tuning.critic.model"
        );
        assert_eq!(
            runner.request.config.critic.model, "fixture-critic",
            "runner Disabled critic keeps tuning.critic.model"
        );
        // Planner disabled on all three production paths.
        assert!(
            pack.request.config.planner.prompt.is_none()
                && pack.request.config.planner.prompt_path.is_none()
        );
        assert!(
            acp.request.config.planner.prompt.is_none()
                && acp.request.config.planner.prompt_path.is_none()
        );
        assert!(
            runner.request.config.planner.prompt.is_none()
                && runner.request.config.planner.prompt_path.is_none()
        );

        // Repair policies.
        assert!(
            pack.request.config.repair.is_some(),
            "pack mirrors coder for repair"
        );
        assert_eq!(
            pack.provenance.source_of("repair"),
            Some("surface.mirror_coder")
        );
        assert!(
            runner.request.config.repair.is_none(),
            "runner has no repair"
        );
        assert_eq!(runner.provenance.source_of("repair"), Some("surface.none"));

        // Trace resolution deltas.
        assert_eq!(
            pack.request.config.trace_dir.as_deref(),
            Some("custom-traces")
        );
        assert_eq!(pack.provenance.source_of("trace_dir"), Some("tuning"));
        assert_eq!(
            runner.provenance.source_of("trace_dir"),
            Some("surface.relative_to_workspace")
        );
        let runner_trace = runner.request.config.trace_dir.as_deref().unwrap();
        assert!(
            runner_trace.contains("custom-traces"),
            "runner anchors relative trace_dir to workspace: {runner_trace}"
        );
    }

    #[test]
    fn explore_mode_disables_progress_stall_limits() {
        let tuning = twisted_tuning();
        let ws = PathBuf::from(".");
        let assembled = assemble_production_run(
            &tuning,
            entry::pack_surface(entry::PackSurfaceArgs {
                task: CoderTask::new("e", "explore"),
                workspace_path: ws,
                sandbox: SandboxSpec::HostLocal,
                coder_role: CoderRoleConfig {
                    model: "m".into(),
                    prompt_path: None,
                    prompt: Some("p".into()),
                    temperature: None,
                    max_tokens: None,
                    max_turns: Some(5),
                    reasoning: None,
                },
                mode: CodingMode::Explore,
                command_policy: CommandPolicy::default(),
                path_policy: PathPolicy::default(),
                hashline: HashlineConfig::default(),
            }),
        );
        assert_eq!(
            assembled.request.config.progress.read_only_turn_limit,
            u32::MAX
        );
        assert_eq!(assembled.request.config.progress.same_tool_limit, u32::MAX);
        assert_eq!(assembled.request.config.progress.max_attempts, 1);
        assert!(assembled.request.config.repair.is_none());
        assert_eq!(
            assembled
                .provenance
                .source_of("progress.read_only_turn_limit"),
            Some("mode.explore")
        );
        assert_eq!(
            assembled.provenance.source_of("repair"),
            Some("mode.restricted")
        );
    }

    #[test]
    fn runner_gate_follows_tuning_not_a_hardcoded_false() {
        // Band F residue: coder-runner used to hardcode gate.enabled = false.
        let tuning = CoderTuning {
            gate: CoderGateConfig {
                enabled: true,
                fresh_reviewers: 1,
                ..Default::default()
            },
            ..CoderTuning::default()
        };
        let assembled = assemble_production_run(
            &tuning,
            entry::runner_surface(
                CoderTask::new("g", "goal"),
                PathBuf::from("."),
                None,
                Some(10),
            ),
        );
        assert!(assembled.request.config.gate.enabled);
        assert_eq!(assembled.provenance.source_of("gate"), Some("tuning"));
    }

    #[test]
    fn disabled_critic_keeps_configured_critic_model() {
        let tuning = CoderTuning {
            coder: CoderRoleConfig {
                model: "fixture-coder".into(),
                ..CoderTuning::default().coder
            },
            critic: CoderRoleConfig {
                model: "fixture-critic".into(),
                ..CoderTuning::default().critic
            },
            gate: CoderGateConfig {
                enabled: true,
                fresh_reviewers: 1,
                ..Default::default()
            },
            ..CoderTuning::default()
        };
        let assembled = assemble_production_run(
            &tuning,
            entry::runner_surface(
                CoderTask::new("g", "goal"),
                PathBuf::from("."),
                Some("fixture-coder".into()),
                Some(10),
            ),
        );
        assert_eq!(assembled.request.config.critic.model, "fixture-critic");
        assert!(
            assembled.request.config.critic.prompt.is_none()
                && assembled.request.config.critic.prompt_path.is_none()
        );
        assert_eq!(assembled.request.config.critic.max_turns, Some(0));
        assert_eq!(
            assembled.provenance.source_of("critic.model"),
            Some("tuning")
        );
    }

    #[test]
    fn acp_trace_provenance_distinguishes_tuning_from_fallback() {
        let configured = assemble_production_run(
            &CoderTuning {
                trace_dir: Some("chosen".into()),
                ..CoderTuning::default()
            },
            entry::acp_surface(
                CoderTask::new("t", "goal"),
                PathBuf::from("."),
                None,
                None,
                0,
                Vec::new(),
            ),
        );
        assert_eq!(configured.provenance.source_of("trace_dir"), Some("tuning"));

        let fallback = assemble_production_run(
            &CoderTuning {
                trace_dir: None,
                ..CoderTuning::default()
            },
            entry::acp_surface(
                CoderTask::new("t", "goal"),
                PathBuf::from("."),
                None,
                None,
                0,
                Vec::new(),
            ),
        );
        assert_eq!(
            fallback.provenance.source_of("trace_dir"),
            Some("surface.data_dir_fallback")
        );
    }

    #[test]
    fn assemble_does_not_drop_command_or_path_policy_from_tuning() {
        let tuning = CoderTuning {
            command_policy: CommandPolicy {
                timeout_secs: 77,
                ..Default::default()
            },
            path_policy: PathPolicy {
                read_max_bytes: 88,
                ..Default::default()
            },
            ..CoderTuning::default()
        };
        let assembled = assemble_production_run(
            &tuning,
            entry::runner_surface(CoderTask::new("p", "g"), PathBuf::from("."), None, None),
        );
        assert_eq!(assembled.request.config.command_policy.timeout_secs, 77);
        assert_eq!(assembled.request.config.path_policy.read_max_bytes, 88);
        assert_eq!(
            assembled.provenance.source_of("command_policy"),
            Some("tuning")
        );
        assert_eq!(
            assembled.provenance.source_of("path_policy"),
            Some("tuning")
        );
    }

    #[test]
    fn empty_model_override_does_not_wipe_the_configured_model() {
        // `if !model.is_empty()` must stay on both coder and repair: deleting the `!` would
        // apply "" and blank the role. Repair is the line mutants actually hit when tuning
        // carries a repair role (ACP FromTuning); coder is the runner path without repair.
        let tuning = CoderTuning {
            coder: CoderRoleConfig {
                model: "keep-me".into(),
                max_turns: Some(9),
                ..CoderTuning::default().coder
            },
            repair: Some(CoderRoleConfig {
                model: "keep-repair".into(),
                max_turns: Some(3),
                ..CoderTuning::default().coder
            }),
            ..CoderTuning::default()
        };
        let runner = assemble_production_run(
            &tuning,
            entry::runner_surface(
                CoderTask::new("m", "goal"),
                PathBuf::from("."),
                Some(String::new()),
                Some(11),
            ),
        );
        assert_eq!(runner.request.config.coder.model, "keep-me");
        assert_eq!(runner.request.config.coder.max_turns, Some(11));

        let acp = assemble_production_run(
            &tuning,
            entry::acp_surface(
                CoderTask::new("m", "goal"),
                PathBuf::from("."),
                Some(String::new()),
                Some(11),
                0,
                Vec::new(),
            ),
        );
        assert_eq!(acp.request.config.coder.model, "keep-me");
        let repair = acp.request.config.repair.expect("acp keeps tuning.repair");
        assert_eq!(
            repair.model, "keep-repair",
            "empty model override must not wipe the repair model"
        );
    }

    #[test]
    fn acp_default_empty_path_policy_replaces_empty_allow_write_globs() {
        // PathPolicy::default() has non-empty write globs; an empty allow_write_globs list from
        // tuning is the "unset" wire form ACP used to treat as lock-down by accident.
        let empty = CoderTuning {
            path_policy: PathPolicy {
                allow_write_globs: Vec::new(),
                read_max_bytes: 999,
                ..Default::default()
            },
            ..CoderTuning::default()
        };
        let expanded = assemble_production_run(
            &empty,
            entry::acp_surface(
                CoderTask::new("p", "g"),
                PathBuf::from("."),
                None,
                Some(5),
                0,
                Vec::new(),
            ),
        );
        assert!(
            !expanded
                .request
                .config
                .path_policy
                .allow_write_globs
                .is_empty(),
            "ACP must expand empty allow_write_globs to PathPolicy::default()"
        );
        assert_eq!(
            expanded.provenance.source_of("path_policy"),
            Some("surface.default_empty_path_policy")
        );

        // Non-empty globs must *not* be rewritten — `&&` must not become `||`, or every ACP run
        // would discard a configured path_policy.
        let set = CoderTuning {
            path_policy: PathPolicy {
                allow_write_globs: vec!["src/**".into()],
                read_max_bytes: 777,
                ..Default::default()
            },
            ..CoderTuning::default()
        };
        let kept = assemble_production_run(
            &set,
            entry::acp_surface(
                CoderTask::new("p", "g"),
                PathBuf::from("."),
                None,
                Some(5),
                0,
                Vec::new(),
            ),
        );
        assert_eq!(
            kept.request.config.path_policy.allow_write_globs,
            vec!["src/**".to_string()]
        );
        assert_eq!(kept.request.config.path_policy.read_max_bytes, 777);
        assert_eq!(kept.provenance.source_of("path_policy"), Some("tuning"));
    }

    #[test]
    fn provenance_record_is_readable_by_source_of() {
        let mut p = AssemblyProvenance::default();
        p.record("gate", "tuning");
        assert_eq!(p.source_of("gate"), Some("tuning"));
        assert_eq!(p.source_of("missing"), None);
        assert_eq!(p.entries().count(), 1);
    }

    /// Every surface field a caller passes must survive into the surface. A dropped field
    /// silently falls back to `Default` — the run then reads the wrong workspace, policy, or
    /// trace location with nothing visibly wrong.
    #[test]
    fn pack_surface_carries_every_argument() {
        let ws = PathBuf::from("/tmp/pack-ws");
        let hashline = HashlineConfig {
            enabled: false,
            hash_length: 3,
        };
        let command_policy = CommandPolicy {
            timeout_secs: 11,
            ..Default::default()
        };
        let path_policy = PathPolicy {
            allow_write_globs: vec!["custom/**".to_string()],
            ..Default::default()
        };
        let surface = entry::pack_surface(entry::PackSurfaceArgs {
            task: CoderTask::new("t1", "desc"),
            workspace_path: ws.clone(),
            sandbox: SandboxSpec::HostLocal,
            coder_role: CoderRoleConfig {
                model: "m".into(),
                ..CoderRoleConfig::default()
            },
            mode: CodingMode::Normal,
            command_policy: command_policy.clone(),
            path_policy: path_policy.clone(),
            hashline: hashline.clone(),
        });
        assert_eq!(surface.workspace_path, ws);
        assert_eq!(surface.workspace.root, ws.to_string_lossy());
        assert_eq!(surface.sandbox, SandboxSpec::HostLocal);
        assert_eq!(surface.command_policy, Some(command_policy));
        assert_eq!(surface.path_policy, Some(path_policy));
        assert_eq!(surface.hashline, Some(hashline));
        assert_eq!(surface.empty_verifiers, EmptyVerifiersPolicy::LeaveEmpty);
        assert_eq!(surface.trace_dir, TraceDirPolicy::AsConfigured);
        assert!(surface.disable_planner);
        assert_eq!(surface.critic, CriticPolicy::Disabled);
        assert_eq!(surface.repair, RepairPolicy::MirrorCoder);
    }

    #[test]
    fn acp_surface_carries_every_argument() {
        let surface = entry::acp_surface(
            CoderTask::new("t2", "desc"),
            PathBuf::from("/tmp/acp-ws"),
            Some("model-x".into()),
            Some(9),
            4,
            vec!["earlier".into()],
        );
        assert_eq!(surface.workspace_path, PathBuf::from("/tmp/acp-ws"));
        assert_eq!(surface.workspace.root, "/tmp/acp-ws");
        assert_eq!(surface.sandbox, SandboxSpec::HostLocal);
        assert_eq!(surface.model_override.as_deref(), Some("model-x"));
        assert_eq!(surface.max_turns, Some(9));
        assert_eq!(surface.attempt, 4);
        assert_eq!(surface.prior_feedback, vec!["earlier".to_string()]);
        assert_eq!(surface.repair, RepairPolicy::FromTuning);
        assert_eq!(
            surface.empty_verifiers,
            EmptyVerifiersPolicy::DefaultForWorkspace
        );
        assert_eq!(surface.trace_dir, TraceDirPolicy::DataDirFallback);
        assert!(surface.default_empty_path_policy);
        assert!(surface.disable_planner);
    }

    #[test]
    fn runner_surface_carries_every_argument() {
        let surface = entry::runner_surface(
            CoderTask::new("t3", "desc"),
            PathBuf::from("/tmp/runner-ws"),
            Some("runner-model".into()),
            Some(7),
        );
        assert_eq!(surface.workspace_path, PathBuf::from("/tmp/runner-ws"));
        assert_eq!(surface.workspace.root, "/tmp/runner-ws");
        assert_eq!(surface.sandbox, SandboxSpec::HostLocal);
        assert_eq!(surface.model_override.as_deref(), Some("runner-model"));
        assert_eq!(surface.max_turns, Some(7));
        assert_eq!(surface.repair, RepairPolicy::None);
        assert_eq!(
            surface.empty_verifiers,
            EmptyVerifiersPolicy::DefaultForWorkspace
        );
        assert_eq!(surface.trace_dir, TraceDirPolicy::RelativeToWorkspace);
        assert!(surface.disable_planner);
    }

    /// A surface-supplied hashline wins even when it differs from tuning — that is the whole
    /// point of the override (explore mode forces hashing off).
    #[test]
    fn a_surface_hashline_overrides_tuning() {
        let tuning = twisted_tuning(); // hashline enabled / length 8
        let mut assembly = AssemblyState {
            config: tuning.run_config(),
            provenance: AssemblyProvenance::default(),
        };
        let forced_off = HashlineConfig {
            enabled: false,
            hash_length: 1,
        };
        apply_hashline(Some(forced_off.clone()), &mut assembly);
        assert_eq!(assembly.config.hashline, forced_off);
        apply_hashline(None, &mut assembly);
        assert_eq!(
            assembly.config.hashline, forced_off,
            "None means leave what is there"
        );
    }
}
