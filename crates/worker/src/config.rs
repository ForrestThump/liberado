//! Worker settings and the config-stack reads the coding pack needs.
//!
//! Both machines run full Liberado config stacks (plan §4): the worker's own
//! `tuning.toml` / `topology.toml` still apply, so delegation can narrow but never widen
//! what the worker grants. The readers here mirror `liberado-coder-run`'s semantics —
//! same files, same env overrides, same failure behavior (`[coder]` that does not parse
//! fails the run instead of silently defaulting).

use std::path::{Path, PathBuf};

use liberado_coder_core::CoderTuning;
use liberado_config_loader::Topology;

#[derive(Debug, Clone)]
pub struct WorkerSettings {
    /// Bind address for the control plane.
    pub bind: String,
    /// Bearer token every route requires.
    pub token: String,
    /// Data root; task queue lives under `<data>/delegate/tasks`.
    pub data_dir: PathBuf,
    /// Config directory holding topology.toml / tuning.toml (optional).
    pub config_dir: Option<PathBuf>,
    /// Model override; None keeps the tuning's role model.
    pub model: Option<String>,
    /// Forge base URL (Gitea). None → submits are rejected honestly: no PR is possible.
    pub forge_url: Option<String>,
    /// Forge API token.
    pub forge_token: String,
    /// Accept private-CA / self-signed certificates on the forge URL. Opt-in for LAN
    /// forges; git's equivalent is scoped separately via GIT_CONFIG_GLOBAL.
    pub forge_insecure_tls: bool,
    /// Base for resolving relative repository paths to clone URLs.
    pub clone_base_url: Option<String>,
    pub max_concurrent: usize,
    /// How long `ask_delegator` parks before falling back to the question's declared
    /// default (or refusing, when there is none).
    pub question_timeout_secs: u64,
    /// Open questions allowed per task before further asks are refused and the task
    /// is marked blocked for the delegator's attention.
    pub max_open_questions: u32,
}

impl WorkerSettings {
    pub fn tasks_dir(&self) -> PathBuf {
        self.data_dir.join("delegate").join("tasks")
    }

    pub fn repos_dir(&self) -> PathBuf {
        self.data_dir.join("delegate").join("repos")
    }

    pub fn worktrees_dir(&self) -> PathBuf {
        self.data_dir.join("delegate").join("worktrees")
    }

    /// Resolve a TaskSpec.repository ("OWNER/REPO" or an absolute URL) to a clone URL.
    pub fn clone_url(&self, repository: &str) -> String {
        if repository.contains("://") || repository.starts_with('/') {
            return repository.to_string();
        }
        match &self.clone_base_url {
            Some(base) => format!("{}/{}.git", base.trim_end_matches('/'), repository),
            // Fall back to the forge itself, the common homelab shape.
            None => match &self.forge_url {
                Some(forge) => format!("{}/{repository}.git", forge.trim_end_matches('/')),
                None => repository.to_string(),
            },
        }
    }
}

/// Read `[coder]` from `<config-dir>/tuning.toml` into the pack's tuning type. A missing
/// file is the default tuning; a file whose `[coder]` does not parse is a hard error —
/// silently dropping fields once offered 21 tools instead of 3 on a live compare.
pub fn read_tuning(config_dir: Option<&Path>) -> Result<CoderTuning, String> {
    let Some(dir) = config_dir else {
        return Ok(CoderTuning::default());
    };
    let path = dir.join("tuning.toml");
    if !path.exists() {
        return Ok(CoderTuning::default());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let value: toml::Value = raw
        .parse()
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    CoderTuning::from_value(value.get("coder"))
        .map_err(|error| format!("invalid [coder] in {}: {error}", path.display()))
}

/// Read `<config-dir>/topology.toml`; missing file means the default topology.
pub fn read_topology(config_dir: Option<&Path>) -> Result<Topology, String> {
    let Some(dir) = config_dir else {
        return Ok(Topology::default());
    };
    let path = dir.join("topology.toml");
    if !path.exists() {
        return Ok(Topology::default());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    toml::from_str(&raw).map_err(|error| format!("parse {}: {error}", path.display()))
}

/// The provider profile the pack will run under: `LIBERADO_CODER_PROVIDER` names one
/// explicitly, otherwise the topology's default provider.
pub fn provider_profile(
    config_dir: Option<&Path>,
    provider_env_override: Option<&str>,
) -> Result<liberado_config_loader::ProviderProfile, String> {
    let topology = read_topology(config_dir)?;
    let name = provider_env_override.unwrap_or(&topology.provider);
    topology
        .providers
        .into_iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| format!("provider '{name}' is not declared in topology.providers"))
}
