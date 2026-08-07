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
    LIBERADO_LOOP_BACKEND, PathPolicy, PipelinePolicy, ProgressPolicy, SandboxSpec, VerifierSpec,
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
    /// Ordered harness checks (`docs/architecture/verifiers.md`). Empty + `validation_command`
    /// still works via legacy single-command resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifiers: Vec<VerifierSpec>,
    #[serde(default)]
    pub verify_policy: PipelinePolicy,
    pub path_policy: PathPolicy,
    pub progress: ProgressPolicy,
}

impl CoderTuning {
    /// Parse and validate the opaque `[coder]` section the config loader carries
    /// (`Tuning::coder`). `None` (section absent) yields validated defaults.
    pub fn from_value(value: Option<&toml::Value>) -> Result<Self> {
        let tuning: Self = match value {
            Some(v) => v
                .clone()
                .try_into()
                .map_err(|e| Error::Config(format!("tuning.coder: {e}")))?,
            None => Self::default(),
        };
        tuning.validate()?;
        Ok(tuning)
    }

    pub fn run_config(&self) -> CoderRunConfig {
        CoderRunConfig {
            backend: self.backend.clone(),
            trace_dir: self.trace_dir.clone(),
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
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.backend.trim().is_empty() {
            return Err(Error::Config(
                "tuning.coder.backend must not be empty".into(),
            ));
        }
        validate_coder_role("planner", &self.planner)?;
        validate_coder_role("coder", &self.coder)?;
        validate_coder_role("critic", &self.critic)?;
        if let Some(repair) = &self.repair {
            validate_coder_role("repair", repair)?;
        }
        // An enabled gate with no fresh reviewers can never reach a strict majority, so *every*
        // attempt would be refuted and no coding goal could ever finish. That is fail-closed
        // working as designed, but as a config it is only ever a mistake — reject it at load
        // time rather than at 3am on attempt 5.
        if self.gate.enabled && self.gate.fresh_reviewers == 0 {
            return Err(Error::Config(
                "tuning.coder.gate.fresh_reviewers must be >= 1 when the gate is enabled \
                 (a gate with no reviewers can never approve)"
                    .into(),
            ));
        }
        for (name, role) in [
            ("gate.gatekeeper", self.gate.gatekeeper.as_ref()),
            ("gate.fresh", self.gate.fresh.as_ref()),
            ("gate.strategist", self.gate.strategist.as_ref()),
        ] {
            if let Some(role) = role {
                validate_single_shot_role(name, role)?;
            }
        }
        if self.command_policy.timeout_secs == 0 {
            return Err(Error::Config(
                "tuning.coder.command_policy.timeout_secs must be >= 1".into(),
            ));
        }
        if self.command_policy.output_max_bytes == 0 {
            return Err(Error::Config(
                "tuning.coder.command_policy.output_max_bytes must be >= 1".into(),
            ));
        }
        if self.path_policy.read_max_bytes == 0 {
            return Err(Error::Config(
                "tuning.coder.path_policy.read_max_bytes must be >= 1".into(),
            ));
        }
        if self.path_policy.search_max_results == 0 {
            return Err(Error::Config(
                "tuning.coder.path_policy.search_max_results must be >= 1".into(),
            ));
        }
        if self.progress.read_only_turn_limit == 0
            || self.progress.same_tool_limit == 0
            || self.progress.validation_repeat_limit == 0
            || self.progress.max_attempts == 0
            || self.progress.event_preview_max_chars == 0
        {
            return Err(Error::Config(
                "tuning.coder.progress limits must all be >= 1".into(),
            ));
        }
        if let Some(command) = &self.validation_command
            && command.program.trim().is_empty()
        {
            return Err(Error::Config(
                "tuning.coder.validation_command.program must not be empty".into(),
            ));
        }
        Ok(())
    }
}

impl Default for CoderTuning {
    fn default() -> Self {
        Self {
            backend: default_coder_backend(),
            trace_dir: default_coder_trace_dir(),
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
        }
    }
}

fn default_coder_backend() -> String {
    LIBERADO_LOOP_BACKEND.to_string()
}

fn default_coder_trace_dir() -> Option<String> {
    Some("coder-traces".to_string())
}

fn default_coder_planner() -> CoderRoleConfig {
    coder_role(
        "deepseek/deepseek-v4-pro",
        "prompts/coder/planner.md",
        Some(8),
    )
}

fn default_coder_role() -> CoderRoleConfig {
    coder_role(
        "deepseek/deepseek-v4-pro",
        "prompts/coder/coder.md",
        Some(30),
    )
}

fn default_coder_critic() -> CoderRoleConfig {
    coder_role(
        "deepseek/deepseek-v4-flash",
        "prompts/coder/critic.md",
        Some(8),
    )
}

fn coder_role(model: &str, prompt_path: &str, max_turns: Option<u32>) -> CoderRoleConfig {
    CoderRoleConfig {
        model: model.to_string(),
        prompt_path: Some(prompt_path.to_string()),
        prompt: None,
        temperature: Some(0.1),
        max_tokens: None,
        max_turns,
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
fn validate_role_identity(name: &str, role: &CoderRoleConfig) -> Result<()> {
    if role.model.trim().is_empty() {
        return Err(Error::Config(format!(
            "tuning.coder.{name}.model must not be empty"
        )));
    }
    let prompt_path_empty = role
        .prompt_path
        .as_deref()
        .map(|path| path.trim().is_empty())
        .unwrap_or(true);
    let prompt_empty = role
        .prompt
        .as_deref()
        .map(|prompt| prompt.trim().is_empty())
        .unwrap_or(true);
    if prompt_path_empty && prompt_empty {
        return Err(Error::Config(format!(
            "tuning.coder.{name} requires prompt_path or prompt"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_section_yields_validated_defaults() {
        let tuning = CoderTuning::from_value(None).unwrap();
        assert_eq!(tuning.backend, LIBERADO_LOOP_BACKEND);
        assert_eq!(tuning.trace_dir.as_deref(), Some("coder-traces"));
    }

    #[test]
    fn parses_overrides_from_raw_value() {
        let value: toml::Value = toml::from_str(
            r#"
            [coder]
            model = "deepseek/deepseek-v4-pro"
            prompt_path = "prompts/custom-coder.md"
            max_turns = 44

            [progress]
            read_only_turn_limit = 5
            same_tool_limit = 4
            validation_repeat_limit = 3
            max_attempts = 2
            event_preview_max_chars = 321
            "#,
        )
        .unwrap();
        let tuning = CoderTuning::from_value(Some(&value)).unwrap();
        assert_eq!(
            tuning.coder.prompt_path.as_deref(),
            Some("prompts/custom-coder.md")
        );
        assert_eq!(tuning.coder.max_turns, Some(44));
        assert_eq!(tuning.progress.event_preview_max_chars, 321);
    }

    #[test]
    fn validation_rejects_missing_role_budget() {
        let mut tuning = CoderTuning::default();
        tuning.coder.max_turns = None;
        let err = tuning.validate().unwrap_err();
        assert!(err.to_string().contains("tuning.coder.coder.max_turns"));
    }

    #[test]
    fn validation_rejects_zero_preview_cap() {
        let mut tuning = CoderTuning::default();
        tuning.progress.event_preview_max_chars = 0;
        let err = tuning.validate().unwrap_err();
        assert!(err.to_string().contains("tuning.coder.progress"));
    }

    #[test]
    fn run_config_clones_all_fields() {
        let tuning = CoderTuning::default();
        let config = tuning.run_config();
        assert_eq!(config.backend, tuning.backend);
        assert_eq!(config.planner.model, tuning.planner.model);
    }

    #[test]
    fn validation_rejects_empty_backend() {
        let tuning = CoderTuning {
            backend: String::new(),
            ..CoderTuning::default()
        };
        assert!(tuning.validate().is_err());
    }

    #[test]
    fn validation_rejects_zero_command_timeout() {
        let mut tuning = CoderTuning::default();
        tuning.command_policy.timeout_secs = 0;
        let err = tuning.validate().unwrap_err();
        assert!(err.to_string().contains("command_policy.timeout_secs"));
    }

    #[test]
    fn validation_rejects_zero_command_output_max_bytes() {
        let mut tuning = CoderTuning::default();
        tuning.command_policy.output_max_bytes = 0;
        let err = tuning.validate().unwrap_err();
        assert!(err.to_string().contains("command_policy.output_max_bytes"));
    }

    #[test]
    fn validation_rejects_zero_path_read_max_bytes() {
        let mut tuning = CoderTuning::default();
        tuning.path_policy.read_max_bytes = 0;
        let err = tuning.validate().unwrap_err();
        assert!(err.to_string().contains("path_policy.read_max_bytes"));
    }

    #[test]
    fn validation_rejects_zero_path_search_max_results() {
        let mut tuning = CoderTuning::default();
        tuning.path_policy.search_max_results = 0;
        let err = tuning.validate().unwrap_err();
        assert!(err.to_string().contains("path_policy.search_max_results"));
    }

    #[test]
    fn validation_rejects_gate_enabled_with_zero_reviewers() {
        let mut tuning = CoderTuning::default();
        tuning.gate.enabled = true;
        tuning.gate.fresh_reviewers = 0;
        let err = tuning.validate().unwrap_err();
        assert!(err.to_string().contains("fresh_reviewers"));
    }

    #[test]
    fn validation_allows_gate_disabled_with_zero_reviewers() {
        let mut tuning = CoderTuning::default();
        tuning.gate.enabled = false;
        tuning.gate.fresh_reviewers = 0;
        assert!(tuning.validate().is_ok());
    }

    #[test]
    fn validation_allows_gate_enabled_with_reviewers() {
        let mut tuning = CoderTuning::default();
        tuning.gate.enabled = true;
        tuning.gate.fresh_reviewers = 1;
        // Need a complete role config for gatekeeper too.
        tuning.gate.gatekeeper = Some(CoderRoleConfig {
            model: "test-model".into(),
            prompt_path: None,
            prompt: Some("gatekeeper".into()),
            temperature: None,
            max_tokens: None,
            max_turns: None,
        });
        assert!(tuning.validate().is_ok());
    }

    #[test]
    fn validation_rejects_progress_zero_fields_individually() {
        let fields = [
            ("read_only_turn_limit", 0, 1, 1, 1, 1),
            ("same_tool_limit", 1, 0, 1, 1, 1),
            ("validation_repeat_limit", 1, 1, 0, 1, 1),
            ("max_attempts", 1, 1, 1, 0, 1),
            ("event_preview_max_chars", 1, 1, 1, 1, 0),
        ];
        for (name, read_only, same_tool, val_repeat, max_att, preview) in &fields {
            let tuning = CoderTuning {
                progress: ProgressPolicy {
                    read_only_turn_limit: *read_only,
                    same_tool_limit: *same_tool,
                    validation_repeat_limit: *val_repeat,
                    max_attempts: *max_att,
                    event_preview_max_chars: *preview,
                },
                ..CoderTuning::default()
            };
            let err = tuning.validate().unwrap_err();
            assert!(
                err.to_string().contains("progress"),
                "progress field {name} = 0 should be rejected"
            );
        }
    }

    #[test]
    fn validate_role_identity_rejects_empty_model() {
        let role = CoderRoleConfig {
            model: "  ".into(),
            prompt_path: None,
            prompt: Some("prompt".into()),
            temperature: None,
            max_tokens: None,
            max_turns: None,
        };
        assert!(validate_role_identity("test", &role).is_err());
    }

    #[test]
    fn validate_role_identity_rejects_empty_prompt_and_path() {
        let role = CoderRoleConfig {
            model: "m".into(),
            prompt_path: None,
            prompt: None,
            temperature: None,
            max_tokens: None,
            max_turns: None,
        };
        assert!(validate_role_identity("test", &role).is_err());
    }

    #[test]
    fn validate_role_identity_accepts_model_with_prompt() {
        let role = CoderRoleConfig {
            model: "m".into(),
            prompt_path: None,
            prompt: Some("p".into()),
            temperature: None,
            max_tokens: None,
            max_turns: None,
        };
        assert!(validate_role_identity("test", &role).is_ok());
    }

    #[test]
    fn validate_single_shot_role_delegates_to_role_identity() {
        let bad = CoderRoleConfig {
            model: String::new(),
            prompt_path: None,
            prompt: None,
            temperature: None,
            max_tokens: None,
            max_turns: None,
        };
        assert!(validate_single_shot_role("x", &bad).is_err());
    }
}
