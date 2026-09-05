//! The `[coder]` tuning section — parsed and validated by the pack, not by the config stack.
//!
//! Moved here from `liberado-config-loader` (2026-07-11 alignment audit): the config loader keeps
//! the `[coder]` section of `tuning.toml` as an opaque `toml::Value`, and this module turns it
//! into a validated [`CoderTuning`]. That keeps the pack/kernel layering honest — config-loader
//! parses *shape*, domain packs own their *vocabulary* (design rule: "domain packs load their own
//! role/policy sections"). Composition roots call [`CoderTuning::from_value`] at boot so the
//! Decision-14 fail-fast property is preserved: an invalid `[coder]` section still fails at
//! startup, just in the pack's parser instead of the loader's.

use liberado_common::{Error, Result};

use crate::{
    CoderCommandConfig, CoderGateConfig, CoderRoleConfig, CoderRunConfig, CommandPolicy,
    ControlPlaneConfig, EditConfig, HashlineConfig, LIBERADO_LOOP_BACKEND, PathPolicy,
    PipelinePolicy, ProgressPolicy, SandboxSpec, SessionCriticConfig, VerifierSpec,
    WorkspaceBuildConfig,
};
use serde::{Deserialize, Serialize};

/// Rust-native coding backend tunables. The shape mirrors `CoderRunConfig`, but this type owns
/// defaults and load-time validation so PR dispatch, TUI clients, and evals can all consume one
/// resolved backend contract instead of each hand-building their own.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoderTuning {
    #[serde(default = "default_coder_backend")]
    pub backend: String,
    #[serde(
        default = "default_coder_trace_dir",
        skip_serializing_if = "Option::is_none"
    )]
    pub trace_dir: Option<String>,
    /// Which trace formats to write, from `[coder] trace_formats`.
    ///
    /// `native` is the canonical record and is always written when tracing is on — it is the only
    /// format with a slot for the harness's own decisions (which tools were *offered* on a turn,
    /// which a guard withdrew, why a run ended). The exporters exist so a run can be compared
    /// against another harness on the same task, and are deliberately lossy views of the native
    /// record rather than a replacement for it: an export can always be regenerated, so nothing is
    /// locked into a third party's schema.
    #[serde(default = "default_trace_formats")]
    pub trace_formats: Vec<TraceFormat>,
    #[serde(default = "default_coder_planner")]
    pub planner: CoderRoleConfig,
    #[serde(default = "default_coder_role")]
    pub coder: CoderRoleConfig,
    #[serde(default = "default_coder_critic")]
    pub critic: CoderRoleConfig,
    /// `[tuning.coder.gate]` — the completion gate (S1). Absent table = off, single-critic path.
    #[serde(default)]
    pub gate: CoderGateConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<CoderRoleConfig>,
    pub sandbox: SandboxSpec,
    pub command_policy: CommandPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_command: Option<CoderCommandConfig>,
    /// Ordered harness checks (`docs/spec/architecture/verifiers.md`). Empty + `validation_command`
    /// still works via legacy single-command resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifiers: Vec<VerifierSpec>,
    #[serde(default)]
    pub verify_policy: PipelinePolicy,
    pub path_policy: PathPolicy,
    pub progress: ProgressPolicy,
    /// `[tuning.coder.hashline]` — line-anchored hashline edit mode (default off).
    #[serde(default)]
    pub hashline: HashlineConfig,
    /// `[coder] prompt_dir` — directory holding the harness prompt files.
    ///
    /// Unset means `prompts/coder` under the working directory, and a missing file means the
    /// copy compiled into the binary. See [`crate::prompts`] for why both exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_dir: Option<String>,
    /// `[coder.session_critic]` — post-run honesty review. Off by default.
    #[serde(default)]
    pub session_critic: SessionCriticConfig,
    /// `[coder.edit]` — anchor matching for `edit_file`.
    #[serde(default)]
    pub edit: EditConfig,
    /// `[coder.workspace]` — build cache and pre-run warm-up.
    ///
    /// `rename` because the field is `workspace_build` and the documented key is `workspace`.
    /// Without it serde looked for `[coder.workspace_build]`, so the section every doc and the
    /// example config told an operator to write parsed into defaults and changed nothing —
    /// measured: a run configured with a shared cache filled its own worktree with 17.6 GB while
    /// the shared directory stayed empty.
    #[serde(default, rename = "workspace")]
    pub workspace_build: WorkspaceBuildConfig,
    /// `[coder] offered_tools` — names the model may call. `None` = the full pack catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered_tools: Option<Vec<String>>,
    /// `[tuning.coder.repo_map]` — Aider-style repository map for cold-start context.
    #[serde(default)]
    pub repo_map: RepoMapConfig,
    /// External coding workers and the default selection policy.
    #[serde(default)]
    pub control_plane: ControlPlaneConfig,
}

/// Configuration for the Aider-style repository map feature.
/// Lives under `[tuning.coder.repo_map]` in tuning.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoMapConfig {
    /// Enable the repo map.  When false the feature is completely off.
    pub enabled: bool,
    /// Route task-named paths and symbols into a bounded evidence block before global ranking.
    /// Off by default until controlled comparisons show an accepted-result benefit.
    pub task_aware: bool,
    /// Approximate token budget for the rendered map (in tokens).
    pub max_map_tokens: usize,
    /// Skip the map entirely when the workspace has fewer than this many source files.
    pub min_source_files: usize,
}

impl Default for RepoMapConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            task_aware: false,
            max_map_tokens: 1024,
            min_source_files: 20,
        }
    }
}

impl CoderTuning {
    /// Parse and validate the opaque `[coder]` section the config loader carries
    /// (`Tuning::coder`). `None` (section absent) yields validated defaults.
    pub fn from_value(value: Option<&toml::Value>) -> Result<Self> {
        let mut tuning: Self = match value {
            Some(v) => v
                .clone()
                .try_into()
                .map_err(|e| Error::Config(format!("tuning.coder: {e}")))?,
            None => Self::default(),
        };
        // A present `[coder.coder]` / `[coder.critic]` table replaces the whole role.
        // Serde then fills omitted fields with `None`, and validate treats
        // `max_turns: None` as 0 — so a file that only pins the critic model (the
        // documented "uncomment what you want to change" shape) killed every
        // process that loads tuning, including `stdio_smoke` via walk-up from
        // `target/debug/liberado-acp.exe`. Restore prompt path and turn budget
        // only when the operator did not set them.
        apply_role_prompt_defaults(&mut tuning);
        apply_role_turn_defaults(&mut tuning);
        tuning.validate()?;
        Ok(tuning)
    }

    pub fn run_config(&self) -> CoderRunConfig {
        CoderRunConfig {
            backend: self.backend.clone(),
            trace_dir: self.trace_dir.clone(),
            trace_formats: self.trace_formats.clone(),
            planner: self.planner.clone(),
            coder: self.coder.clone(),
            critic: self.critic.clone(),
            gate: self.gate.clone(),
            repair: self.repair.clone(),
            sandbox: self.sandbox.clone(),
            command_policy: self.command_policy.clone(),
            validation_command: self.validation_command.clone(),
            verifiers: self.verifiers.clone(),
            verify_policy: self.verify_policy.clone(),
            path_policy: self.path_policy.clone(),
            progress: self.progress.clone(),
            hashline: self.hashline.clone(),
            session_critic: self.session_critic.clone(),
            prompt_dir: self.prompt_dir.clone(),
            edit: self.edit.clone(),
            workspace_build: self.workspace_build.clone(),
            offered_tools: self.offered_tools.clone(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        validate_tuning_backend(&self.backend)?;
        validate_coder_role("planner", &self.planner)?;
        validate_coder_role("coder", &self.coder)?;
        validate_coder_role("critic", &self.critic)?;
        if let Some(repair) = &self.repair {
            validate_coder_role("repair", repair)?;
        }
        validate_tuning_gate(&self.gate)?;
        validate_tuning_command_policy(&self.command_policy)?;
        validate_tuning_path_policy(&self.path_policy)?;
        validate_tuning_progress(&self.progress)?;
        validate_tuning_validation_command(self.validation_command.as_ref())?;
        validate_hashline_and_control_plane(&self.hashline, &self.control_plane)?;
        Ok(())
    }
}

fn validate_hashline_and_control_plane(
    hashline: &HashlineConfig,
    control_plane: &ControlPlaneConfig,
) -> Result<()> {
    hashline
        .validate()
        .map_err(|e| Error::Config(format!("tuning.coder.{e}")))?;
    control_plane
        .validate()
        .map_err(|e| Error::Config(format!("tuning.coder.control_plane: {e}")))?;
    Ok(())
}

fn validate_tuning_backend(backend: &str) -> Result<()> {
    if backend.trim().is_empty() {
        return Err(Error::Config(
            "tuning.coder.backend must not be empty".into(),
        ));
    }
    Ok(())
}

/// An enabled gate with no fresh reviewers can never reach a strict majority, so *every* attempt
/// would be refuted and no coding goal could ever finish. That is fail-closed working as designed,
/// but as a config it is only ever a mistake — reject it at load time rather than at 3am on
/// attempt 5 — and the gatekeeper/fresh/strategist roles must be single-shot.
fn validate_tuning_gate(gate: &CoderGateConfig) -> Result<()> {
    if gate.enabled && gate.fresh_reviewers == 0 {
        return Err(Error::Config(
            "tuning.coder.gate.fresh_reviewers must be >= 1 when the gate is enabled \
             (a gate with no reviewers can never approve)"
                .into(),
        ));
    }
    for (name, role) in [
        ("gate.gatekeeper", gate.gatekeeper.as_ref()),
        ("gate.fresh", gate.fresh.as_ref()),
        ("gate.strategist", gate.strategist.as_ref()),
    ] {
        if let Some(role) = role {
            validate_single_shot_role(name, role)?;
        }
    }
    Ok(())
}

fn validate_tuning_command_policy(policy: &CommandPolicy) -> Result<()> {
    if policy.timeout_secs == 0 {
        return Err(Error::Config(
            "tuning.coder.command_policy.timeout_secs must be >= 1".into(),
        ));
    }
    if policy.output_max_bytes == 0 {
        return Err(Error::Config(
            "tuning.coder.command_policy.output_max_bytes must be >= 1".into(),
        ));
    }
    Ok(())
}

fn validate_tuning_path_policy(policy: &PathPolicy) -> Result<()> {
    if policy.read_max_bytes == 0 {
        return Err(Error::Config(
            "tuning.coder.path_policy.read_max_bytes must be >= 1".into(),
        ));
    }
    if policy.search_max_results == 0 {
        return Err(Error::Config(
            "tuning.coder.path_policy.search_max_results must be >= 1".into(),
        ));
    }
    Ok(())
}

fn validate_tuning_progress(policy: &ProgressPolicy) -> Result<()> {
    if policy.read_only_turn_limit == 0
        || policy.same_tool_limit == 0
        || policy.validation_repeat_limit == 0
        || policy.max_attempts == 0
        || policy.event_preview_max_chars == 0
    {
        return Err(Error::Config(
            "tuning.coder.progress limits must all be >= 1".into(),
        ));
    }
    Ok(())
}

fn validate_tuning_validation_command(command: Option<&CoderCommandConfig>) -> Result<()> {
    if let Some(command) = command
        && command.program.trim().is_empty()
    {
        return Err(Error::Config(
            "tuning.coder.validation_command.program must not be empty".into(),
        ));
    }
    Ok(())
}

impl Default for CoderTuning {
    fn default() -> Self {
        Self {
            backend: default_coder_backend(),
            trace_dir: default_coder_trace_dir(),
            trace_formats: default_trace_formats(),
            planner: default_coder_planner(),
            coder: default_coder_role(),
            critic: default_coder_critic(),
            gate: CoderGateConfig::default(),
            repair: None,
            sandbox: SandboxSpec::HostLocal,
            command_policy: CommandPolicy::default(),
            validation_command: None,
            verifiers: Vec::new(),
            verify_policy: PipelinePolicy::default(),
            path_policy: PathPolicy::default(),
            progress: ProgressPolicy::default(),
            hashline: HashlineConfig::default(),
            session_critic: SessionCriticConfig::default(),
            edit: EditConfig::default(),
            workspace_build: WorkspaceBuildConfig::default(),
            prompt_dir: None,
            offered_tools: None,
            repo_map: RepoMapConfig::default(),
            control_plane: ControlPlaneConfig::default(),
        }
    }
}

fn default_coder_backend() -> String {
    LIBERADO_LOOP_BACKEND.to_string()
}

/// A trace serialization. See [`CoderTuning::trace_formats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TraceFormat {
    /// This system's own record: every `CoderEvent`, including guard decisions and the tool
    /// catalog per turn. Lossless.
    Native,
    /// A flat OpenAI-style message list (`system`/`user`/`assistant`/`tool`), the shape most other
    /// harnesses persist — Kilo Code writes essentially this as `api_conversation_history.json`,
    /// and OpenHands trajectories are the same shape. Chosen as the first exporter because it
    /// makes a same-task, same-model comparison a near-direct diff.
    ///
    /// Lossy by construction: the message list has nowhere to put `tools_offered` or a guard
    /// strike, which is exactly why it is an export and not the storage format.
    OpenaiMessages,
}

fn default_trace_formats() -> Vec<TraceFormat> {
    vec![TraceFormat::Native]
}

fn default_coder_trace_dir() -> Option<String> {
    Some("coder-traces".to_string())
}

fn default_coder_planner() -> CoderRoleConfig {
    coder_role("deepseek-v4-pro", "prompts/coder/planner.md", Some(8))
}

fn default_coder_role() -> CoderRoleConfig {
    coder_role("deepseek-v4-pro", "prompts/coder/coder.md", Some(50))
}

fn default_coder_critic() -> CoderRoleConfig {
    coder_role("deepseek-v4-flash", "prompts/coder/critic.md", Some(8))
}

fn coder_role(model: &str, prompt_path: &str, max_turns: Option<u32>) -> CoderRoleConfig {
    CoderRoleConfig {
        model: model.to_string(),
        prompt_path: Some(prompt_path.to_string()),
        prompt: None,
        temperature: Some(0.1),
        max_tokens: None,
        max_turns,
        reasoning: None,
    }
}

/// Validate a role that makes exactly **one completion call** — the completion gate's reviewers and
/// strategist. Identical to [`validate_coder_role`] minus the `max_turns` requirement: these roles
/// never enter the executor's agent loop, so a turn limit is meaningless for them and demanding one
/// would make the documented `[coder.gate.*]` config fail at boot.
fn validate_single_shot_role(name: &str, role: &CoderRoleConfig) -> Result<()> {
    validate_role_identity(name, role)
}

fn validate_coder_role(name: &str, role: &CoderRoleConfig) -> Result<()> {
    validate_role_identity(name, role)?;
    if role.max_turns.unwrap_or(0) == 0 {
        return Err(Error::Config(format!(
            "tuning.coder.{name}.max_turns must be >= 1"
        )));
    }
    Ok(())
}

/// Model + prompt requirements shared by every role.
fn role_has_no_prompt(role: &CoderRoleConfig) -> bool {
    let path_empty = role
        .prompt_path
        .as_deref()
        .map(|path| path.trim().is_empty())
        .unwrap_or(true);
    let prompt_empty = role
        .prompt
        .as_deref()
        .map(|prompt| prompt.trim().is_empty())
        .unwrap_or(true);
    path_empty && prompt_empty
}

fn apply_role_prompt_defaults(tuning: &mut CoderTuning) {
    if role_has_no_prompt(&tuning.planner) {
        tuning.planner.prompt_path = default_coder_planner().prompt_path;
    }
    if role_has_no_prompt(&tuning.coder) {
        tuning.coder.prompt_path = default_coder_role().prompt_path;
    }
    if role_has_no_prompt(&tuning.critic) {
        tuning.critic.prompt_path = default_coder_critic().prompt_path;
    }
}

/// Restore each role's default turn budget when a partial table omitted it.
///
/// `validate_coder_role` rejects `None` (`unwrap_or(0) == 0`). An explicit
/// `max_turns = 0` stays `Some(0)` and still fails validation.
fn apply_role_turn_defaults(tuning: &mut CoderTuning) {
    if tuning.planner.max_turns.is_none() {
        tuning.planner.max_turns = default_coder_planner().max_turns;
    }
    if tuning.coder.max_turns.is_none() {
        tuning.coder.max_turns = default_coder_role().max_turns;
    }
    if tuning.critic.max_turns.is_none() {
        tuning.critic.max_turns = default_coder_critic().max_turns;
    }
}

fn validate_role_identity(name: &str, role: &CoderRoleConfig) -> Result<()> {
    if role.model.trim().is_empty() {
        return Err(Error::Config(format!(
            "tuning.coder.{name}.model must not be empty"
        )));
    }
    if role_has_no_prompt(role) {
        return Err(Error::Config(format!(
            "tuning.coder.{name} requires prompt_path or prompt"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "tuning_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tuning_documented_key_tests.rs"]
mod documented_key_tests;

#[cfg(test)]
#[path = "tuning_survivor_tests.rs"]
mod survivor_tests;
