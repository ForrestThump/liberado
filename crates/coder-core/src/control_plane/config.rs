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
}

impl Default for ControlPlaneConfig {
    fn default() -> Self {
        Self {
            default_worker: NATIVE_WORKER_ID.into(),
            workers: BTreeMap::new(),
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
