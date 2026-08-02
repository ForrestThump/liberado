//! Runner-local config (`conformance.toml` next to deploy topology/policy).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Default whole-run budget when the TOML key is absent (30 minutes).
pub const DEFAULT_BUDGET_SECS: u64 = 1800;

/// Suite-owned schedule/hook names that P1a must **not** require to have fired.
pub const SUITE_OWNED_NAMES: &[&str] = &["conformance", "conformance-notify"];

#[derive(Debug, Clone, Deserialize)]
pub struct ConformanceConfig {
    /// Daemon base URL. Required — no default pointing at production.
    pub base_url: String,
    /// Whole-run wall-clock budget in seconds. Default [`DEFAULT_BUDGET_SECS`].
    #[serde(default = "default_budget")]
    pub budget_secs: u64,
    /// Absolute (or runner-cwd-relative) path to the vault root — used to check P1b artifacts and
    /// write reports under `conformance/`.
    pub vault_path: PathBuf,
    /// Path to the deployed `topology.toml` (for P1a schedule list + periods). Optional; P1a
    /// skips with a reason when absent.
    #[serde(default)]
    pub topology_path: Option<PathBuf>,
    /// Hook name to fire for P1b/P3. Default `conformance`.
    #[serde(default = "default_hook")]
    pub hook_name: String,
    /// Env var name holding the hook secret (Decision 10 — never the secret itself).
    #[serde(default = "default_hook_secret_ref")]
    pub hook_secret_ref: String,
    /// Session profile name for P4 spawn. Default `conformance`.
    #[serde(default = "default_profile")]
    pub profile_name: String,
    /// Paths to run. Empty / omitted = all non-advisory (P1a–P4). P5 is opt-in via list or flag.
    #[serde(default)]
    pub paths: Vec<String>,
    /// When true, P5 (delegate) participates in the exit code. Default false (advisory).
    #[serde(default)]
    pub advisory_counts: bool,
    /// Per-path timeout in seconds (model-backed paths). Default 600 (10 min).
    #[serde(default = "default_path_timeout")]
    pub path_timeout_secs: u64,
}

fn default_budget() -> u64 {
    DEFAULT_BUDGET_SECS
}
fn default_hook() -> String {
    "conformance".into()
}
fn default_hook_secret_ref() -> String {
    "LIBERADO_HOOK_CONFORMANCE_SECRET".into()
}
fn default_profile() -> String {
    "conformance".into()
}
fn default_path_timeout() -> u64 {
    600
}

impl ConformanceConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let cfg: Self = toml::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
        if cfg.base_url.trim().is_empty() {
            return Err("base_url must be set (no production default)".into());
        }
        Ok(cfg)
    }

    pub fn budget(&self) -> Duration {
        Duration::from_secs(self.budget_secs)
    }

    pub fn path_timeout(&self) -> Duration {
        Duration::from_secs(self.path_timeout_secs)
    }

    pub fn hook_secret(&self) -> Result<String, String> {
        std::env::var(&self.hook_secret_ref).map_err(|_| {
            format!(
                "env var {} is unset — cannot fire the conformance hook",
                self.hook_secret_ref
            )
        })
    }

    /// Whether this schedule/hook name is suite-owned and must be ignored by P1a.
    pub fn is_suite_owned(name: &str) -> bool {
        SUITE_OWNED_NAMES.contains(&name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_owned_names_are_recognized() {
        assert!(ConformanceConfig::is_suite_owned("conformance"));
        assert!(ConformanceConfig::is_suite_owned("conformance-notify"));
        assert!(!ConformanceConfig::is_suite_owned("daily-planning"));
    }

    #[test]
    fn default_budget_is_thirty_minutes() {
        assert_eq!(DEFAULT_BUDGET_SECS, 1800);
    }
}
