//! Cross-platform operator workflows used by the `liberado` command surface.
//!
//! This crate owns stateful operator policy that used to be duplicated across PowerShell and
//! Bash: configuration resolution, deployment plans, artifact validation, process lifecycle, and
//! external-surface installation. Host-specific values are never compiled in. They come from
//! `ops.toml`, resolved by [`OpsConfig::load`].

mod archive;
mod command;
mod config;
mod deploy;
mod dev;
mod paseo;
mod webui_deploy;

pub use command::{CommandPlan, CommandSpec};
pub use config::{DevelopmentConfig, HomelabConfig, OpsConfig, OpsConfigError, PaseoConfig};
pub use deploy::{DeployOptions, deploy_homelab, homelab_plan, latency_homelab, smoke_homelab};
pub use dev::{DevAction, DevOptions, run_dev};
pub use paseo::{PaseoInstallOptions, install_paseo};
pub use webui_deploy::{deploy_webui, webui_plan};

use std::path::{Path, PathBuf};

/// Find the repository root by walking upward from `start`.
pub fn repository_root(start: &Path) -> Result<PathBuf, String> {
    let mut current = start
        .canonicalize()
        .map_err(|error| format!("resolve {}: {error}", start.display()))?;
    loop {
        if current.join("Cargo.toml").is_file() && current.join("crates").is_dir() {
            return Ok(current);
        }
        if !current.pop() {
            return Err("could not find repository root (expected Cargo.toml and crates/)".into());
        }
    }
}
