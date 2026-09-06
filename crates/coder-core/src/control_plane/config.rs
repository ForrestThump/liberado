//! Pack-owned worker registry configuration.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{ControlPlaneError, OpenCodeWorker, OpenCodeWorkerConfig, WorkerPort};

/// Stable id for Liberado's first-party coding backend in worker-selection config.
pub const NATIVE_WORKER_ID: &str = crate::LIBERADO_LOOP_BACKEND;

/// Worker wiring from `[tuning.coder.control_plane]`.
///
/// The config loader keeps this section opaque. `coder-core` owns its vocabulary so adding a
/// harness never adds domain knowledge to the configuration kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControlPlaneConfig {
    /// Worker used when a session profile does not select one.
    pub default_worker: String,
    /// Named external workers. The map key is what profile overrides select.
    pub workers: BTreeMap<String, WorkerAdapterConfig>,
    /// PR-review adapters are declarative in Slices 0/1. No process is started here.
    pub review_workers: BTreeMap<String, ReviewWorkerConfig>,
}

impl Default for ControlPlaneConfig {
    fn default() -> Self {
        Self {
            default_worker: NATIVE_WORKER_ID.into(),
            workers: BTreeMap::new(),
            review_workers: BTreeMap::new(),
        }
    }
}

impl ControlPlaneConfig {
    pub fn validate(&self) -> Result<(), ControlPlaneError> {
        if self.default_worker.trim().is_empty() {
            return Err(ControlPlaneError::InvalidConfig(
                "default_worker must not be empty".into(),
            ));
        }
        if self.default_worker != NATIVE_WORKER_ID
            && !self.workers.contains_key(&self.default_worker)
        {
            return Err(ControlPlaneError::InvalidConfig(format!(
                "default_worker '{}' names no configured worker",
                self.default_worker
            )));
        }
        for (name, worker) in &self.workers {
            if name.trim().is_empty() || name == NATIVE_WORKER_ID {
                return Err(ControlPlaneError::InvalidConfig(format!(
                    "worker name '{name}' is empty or reserved"
                )));
            }
            worker.validate(name)?;
        }
        for (name, worker) in &self.review_workers {
            worker.validate(name)?;
        }
        Ok(())
    }

    pub fn enabled_review_workers(&self) -> impl Iterator<Item = (&str, &ReviewWorkerConfig)> {
        self.review_workers
            .iter()
            .filter_map(|(name, worker)| worker.enabled().then_some((name.as_str(), worker)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewWorkerConfig {
    GrokBuild {
        executable: String,
        enabled: bool,
    },
    Codex {
        executable: String,
        enabled: bool,
    },
    Antigravity {
        executable: String,
        enabled: bool,
        print_timeout: String,
    },
    CursorLocal {
        executable: String,
        enabled: bool,
    },
    OpenaiCompatible {
        base_url: String,
        model: String,
        pricing_policy: String,
        enabled: bool,
    },
}

impl ReviewWorkerConfig {
    pub fn enabled(&self) -> bool {
        match self {
            Self::GrokBuild { enabled, .. }
            | Self::Codex { enabled, .. }
            | Self::Antigravity { enabled, .. }
            | Self::CursorLocal { enabled, .. }
            | Self::OpenaiCompatible { enabled, .. } => *enabled,
        }
    }

    fn validate(&self, name: &str) -> Result<(), ControlPlaneError> {
        let executable = match self {
            Self::GrokBuild { executable, .. }
            | Self::Codex { executable, .. }
            | Self::Antigravity { executable, .. }
            | Self::CursorLocal { executable, .. } => Some(executable),
            Self::OpenaiCompatible {
                base_url,
                model,
                pricing_policy,
                ..
            } => {
                if base_url.trim().is_empty()
                    || model.trim().is_empty()
                    || pricing_policy != "zero_only"
                {
                    return Err(ControlPlaneError::InvalidConfig(format!(
                        "review worker '{name}' must have a URL, model, and zero_only pricing"
                    )));
                }
                None
            }
        };
        if executable.is_some_and(|value| value.trim().is_empty()) {
            return Err(ControlPlaneError::InvalidConfig(format!(
                "review worker '{name}'.executable must not be empty"
            )));
        }
        Ok(())
    }
}

/// One configured external worker implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerAdapterConfig {
    OpenCode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executable: Option<String>,
        #[serde(default = "default_opencode_model")]
        model: String,
        #[serde(default = "default_true")]
        auto_approve: bool,
    },
}

impl WorkerAdapterConfig {
    fn validate(&self, name: &str) -> Result<(), ControlPlaneError> {
        match self {
            Self::OpenCode {
                executable, model, ..
            } => {
                if executable
                    .as_deref()
                    .is_some_and(|value| value.trim().is_empty())
                {
                    return Err(ControlPlaneError::InvalidConfig(format!(
                        "worker '{name}'.executable must not be empty"
                    )));
                }
                if model.trim().is_empty() {
                    return Err(ControlPlaneError::InvalidConfig(format!(
                        "worker '{name}'.model must not be empty"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn build(&self) -> Arc<dyn WorkerPort> {
        match self {
            Self::OpenCode {
                executable,
                model,
                auto_approve,
            } => Arc::new(OpenCodeWorker::new(OpenCodeWorkerConfig {
                executable: executable.clone(),
                model: model.clone(),
                auto_approve: *auto_approve,
            })),
        }
    }
}

fn default_opencode_model() -> String {
    OpenCodeWorkerConfig::default().model
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod review_worker_tests {
    use super::*;

    #[test]
    fn disabled_grok_is_not_admitted() {
        let mut config = ControlPlaneConfig::default();
        config.review_workers.insert(
            "grok-build".into(),
            ReviewWorkerConfig::GrokBuild {
                executable: "/must-not-run/grok".into(),
                enabled: false,
            },
        );
        config.review_workers.insert(
            "codex".into(),
            ReviewWorkerConfig::Codex {
                executable: "codex".into(),
                enabled: true,
            },
        );
        assert_eq!(
            config
                .enabled_review_workers()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["codex"]
        );
    }

    #[test]
    fn free_router_must_be_proved_zero_only() {
        let mut config = ControlPlaneConfig::default();
        config.review_workers.insert(
            "free-router".into(),
            ReviewWorkerConfig::OpenaiCompatible {
                base_url: "http://127.0.0.1:1234/v1".into(),
                model: "auto".into(),
                pricing_policy: "cheap".into(),
                enabled: true,
            },
        );
        assert!(config.validate().is_err());
    }
}
