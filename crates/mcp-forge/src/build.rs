//! Builds one [`McpSource`]: resolves its remote git SHA, skips the build if the lockfile already
//! has it (unless forced), shells out to `cargo install --git`, and verifies the binary landed
//! where `McpTransport::Managed` will look for it.

use std::path::Path;
use std::process::Command;

use liberado_config::managed_binary_path;

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

/// A `git` source's remote SHA, for deciding whether a rebuild is even necessary. `None` for a
/// `path` source — there's no remote ref to check, so `sync_source` always rebuilds it (see
/// [`McpSource::path`]'s doc comment).
fn resolve_source_version(source: &McpSource) -> Result<Option<String>, BuildError> {
    let Some(git) = &source.git else {
        return Ok(None);
    };
    let rev = source.rev.as_deref().unwrap_or("HEAD");
    let output = Command::new("git")
        .args(["ls-remote", git, rev])
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
        .map(Some)
        .ok_or_else(|| BuildError::LsRemoteStatus {
            name: source.name.clone(),
            detail: format!("no ref '{rev}' found (empty ls-remote output)"),
        })
}

/// `cargo install --git <url> [<package>] --root <install_dir>/<name> --locked --force --rev
/// <resolved_sha> [--bin]` for a `git` source, or
/// `cargo install --path <dir> [-p <package>] --root <install_dir>/<name> --locked --force [--bin]`
/// for a `path` one.
///
/// For `--git`, `cargo install` has no `-p`/`--package` flag (unlike `cargo build`) — for a source
/// that's a Cargo workspace, the package to install is selected via the trailing positional `CRATE`
/// argument instead (`cargo install --git <url> <crate-name>`). `--path` installs use the real
/// `-p`/`--package` flag instead — the positional-crate-name form is specific to registry/git
/// installs. `--bin` remains a real flag either way, for picking one binary out of a package that
/// builds more than one.
///
/// `--rev` is always the SHA [`resolve_source_version`] already resolved for a `git` source
/// (`resolved_version` is `None` for `path`, which also skips `--rev` entirely — a plain directory
/// has no git ref concept) — never `source.rev` directly, otherwise `cargo install` would
/// re-resolve the ref itself, and a push to the upstream branch between the two resolutions would
/// silently build a different commit than the one just checked against the lockfile.
///
/// Output is inherited (not captured) — builds run sequentially, so there's no interleaving to
/// worry about, and passing raw `cargo` output through is more useful than re-wrapping it.
fn cargo_install(
    source: &McpSource,
    resolved_version: Option<&str>,
    install_dir: &Path,
) -> Result<(), BuildError> {
    let root = install_dir.join(&source.name);
    let mut cmd = Command::new("cargo");
    cmd.arg("install");

    let describe = if let Some(git) = &source.git {
        cmd.arg("--git").arg(git);
        if let Some(package) = &source.package {
            cmd.arg(package);
        }
        format!("--git {git}")
    } else if let Some(path) = &source.path {
        cmd.arg("--path").arg(path);
        if let Some(package) = &source.package {
            cmd.arg("-p").arg(package);
        }
        format!("--path {path}")
    } else {
        unreachable!("load_sources validates exactly one of git/path is set")
    };

    cmd.arg("--root").arg(&root).arg("--locked").arg("--force");
    if let Some(version) = resolved_version {
        cmd.arg("--rev").arg(version);
    }
    if let Some(bin) = &source.bin {
        cmd.arg("--bin").arg(bin);
    }

    println!("== [{}] cargo install {describe} ==", source.name);
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

/// Sync one source: for a `git` source, skip if already built at the current remote SHA (unless
/// `force`); a `path` source has no remote SHA to check, so it always rebuilds (cargo's own
/// incremental cache keeps a no-op rebuild cheap — see [`McpSource::path`]'s doc comment). Updates
/// `lock` in-memory on success — the caller is responsible for saving it (once, after all sources,
/// so a mid-run failure doesn't lose earlier progress).
pub fn sync_source(
    source: &McpSource,
    install_dir: &Path,
    lock: &mut LockFile,
    force: bool,
) -> Result<SyncOutcome, BuildError> {
    let version = resolve_source_version(source)?;
    if let Some(version) = &version
        && !force
        && lock.built_sha(&source.name) == Some(version.as_str())
    {
        return Ok(SyncOutcome::UpToDate);
    }

    cargo_install(source, version.as_deref(), install_dir)?;

    let expected = managed_binary_path(install_dir, &source.name);
    if !expected.is_file() {
        return Err(BuildError::MissingBinary {
            name: source.name.clone(),
            expected: expected.display().to_string(),
        });
    }

    if let Some(version) = &version {
        lock.record(&source.name, version);
    }
    Ok(SyncOutcome::Built)
}
