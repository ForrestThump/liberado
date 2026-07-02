//! Builds one [`McpSource`]: resolves its remote git SHA, skips the build if the lockfile already
//! has it (unless forced), shells out to `cargo install --git`, and verifies the binary landed
//! where `McpTransport::Managed` will look for it.

use std::path::Path;
use std::process::Command;

use liberado_common::config::managed_binary_path;

use crate::lock::LockFile;
use crate::sources::McpSource;

#[derive(Debug, PartialEq, Eq)]
pub enum SyncOutcome {
    UpToDate,
    Built,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("failed to run `git ls-remote` for '{name}': {source}")]
    LsRemoteIo {
        name: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`git ls-remote` for '{name}' failed: {detail}")]
    LsRemoteStatus { name: String, detail: String },
    #[error("failed to run `cargo install` for '{name}': {source}")]
    CargoInstallIo {
        name: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`cargo install` for '{name}' exited with {code}")]
    CargoInstallStatus { name: String, code: String },
    #[error("'{name}' built successfully but the binary is missing at {expected}")]
    MissingBinary { name: String, expected: String },
}

/// `git ls-remote <url> <rev-or-HEAD>` — the resolved commit SHA for the target rev, without a
/// full clone. Used to decide whether a rebuild is even necessary.
fn resolve_remote_sha(source: &McpSource) -> Result<String, BuildError> {
    let rev = source.rev.as_deref().unwrap_or("HEAD");
    let output = Command::new("git")
        .args(["ls-remote", &source.git, rev])
        .output()
        .map_err(|e| BuildError::LsRemoteIo {
            name: source.name.clone(),
            source: e,
        })?;
    if !output.status.success() {
        return Err(BuildError::LsRemoteStatus {
            name: source.name.clone(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_string)
        .ok_or_else(|| BuildError::LsRemoteStatus {
            name: source.name.clone(),
            detail: format!("no ref '{rev}' found (empty ls-remote output)"),
        })
}

/// `cargo install --git <url> [<package>] --root <install_dir>/<name> --locked --force
/// [--rev][--bin]`.
///
/// `cargo install` has no `-p`/`--package` flag (unlike `cargo build`) — for a git source that's
/// a Cargo workspace, the package to install is selected via the trailing positional `CRATE`
/// argument instead (`cargo install --git <url> <crate-name>`). `--bin` remains a real flag, for
/// picking one binary out of a package that builds more than one.
///
/// Output is inherited (not captured) — builds run sequentially, so there's no interleaving to
/// worry about, and passing raw `cargo` output through is more useful than re-wrapping it.
fn cargo_install(source: &McpSource, install_dir: &Path) -> Result<(), BuildError> {
    let root = install_dir.join(&source.name);
    let mut cmd = Command::new("cargo");
    cmd.arg("install").arg("--git").arg(&source.git);
    if let Some(package) = &source.package {
        cmd.arg(package);
    }
    cmd.arg("--root").arg(&root).arg("--locked").arg("--force");
    if let Some(rev) = &source.rev {
        cmd.arg("--rev").arg(rev);
    }
    if let Some(bin) = &source.bin {
        cmd.arg("--bin").arg(bin);
    }

    println!("== [{}] cargo install --git {} ==", source.name, source.git);
    let status = cmd.status().map_err(|e| BuildError::CargoInstallIo {
        name: source.name.clone(),
        source: e,
    })?;
    if !status.success() {
        return Err(BuildError::CargoInstallStatus {
            name: source.name.clone(),
            code: status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
        });
    }
    Ok(())
}

/// Sync one source: skip if already built at the current remote SHA (unless `force`), otherwise
/// build and verify. Updates `lock` in-memory on success — the caller is responsible for saving
/// it (once, after all sources, so a mid-run failure doesn't lose earlier progress).
pub fn sync_source(
    source: &McpSource,
    install_dir: &Path,
    lock: &mut LockFile,
    force: bool,
) -> Result<SyncOutcome, BuildError> {
    let remote_sha = resolve_remote_sha(source)?;
    if !force && lock.built_sha(&source.name) == Some(remote_sha.as_str()) {
        return Ok(SyncOutcome::UpToDate);
    }

    cargo_install(source, install_dir)?;

    let expected = managed_binary_path(install_dir, &source.name);
    if !expected.is_file() {
        return Err(BuildError::MissingBinary {
            name: source.name.clone(),
            expected: expected.display().to_string(),
        });
    }

    lock.record(&source.name, &remote_sha);
    Ok(SyncOutcome::Built)
}
