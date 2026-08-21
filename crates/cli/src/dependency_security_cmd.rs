//! Locked dependency admission shared by local CI and GitHub CI.

use crate::ci_cmd::{CiLog, run_cmd};

/// Inspect the dependency graph before any later gate compiles third-party code.
pub(crate) fn run(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    run_cmd(
        log,
        "cargo",
        &["metadata", "--locked", "--format-version=1"],
    )?;
    run_cmd(log, "cargo", &["deny", "--locked", "check"])?;
    run_cmd(log, "cargo", &["vet", "--locked"])
}
