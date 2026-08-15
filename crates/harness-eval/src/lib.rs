//! Durable, harness-neutral coding comparisons.
//!
//! This crate owns the comparison job contract, immutable inputs, journal, preflight policy,
//! harness adapters, and worker execution. The `liberado` CLI and the user-context worker are
//! transport surfaces over this library; neither owns comparison policy.

pub mod adapter;
pub mod contract;
pub mod engine;
pub mod job_cli;
pub mod journal;
pub mod legacy;
pub mod preflight;
pub mod transport;
pub mod worker;

use std::io;
use std::path::{Path, PathBuf};

/// Locate the workspace root from the current directory.
pub fn repository_root() -> io::Result<PathBuf> {
    let mut directory = std::env::current_dir()?;
    loop {
        if is_workspace_manifest(&directory.join("Cargo.toml"))? {
            return Ok(directory);
        }
        if !directory.pop() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "could not find a Cargo workspace root",
            ));
        }
    }
}

fn is_workspace_manifest(path: &Path) -> io::Result<bool> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text.lines().any(|line| line.trim() == "[workspace]")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
