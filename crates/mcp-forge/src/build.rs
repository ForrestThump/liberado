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
/// `cargo install --path <dir> [<package>] --root <install_dir>/<name> --locked --force [--bin]`
/// for a `path` one.
///
/// `cargo install` has no `-p`/`--package` flag (unlike `cargo build`) — for a source that's a
/// Cargo workspace, the package to install is selected via the trailing positional `CRATE`
/// argument instead (`cargo install --git <url> <crate-name>` / `cargo install --path <dir>
/// <crate-name>`). `--bin` remains a real flag either way, for picking one binary out of a
/// package that builds more than one.
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
            // `cargo install` has no `-p`/`--package` flag (unlike `cargo build`) — the package
            // is selected via the trailing positional `CRATE` argument for `--git` and `--path`
            // installs alike.
            cmd.arg(package);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockFile;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> std::process::Output {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// A minimal, dependency-free Cargo project that `cargo install` can build. The forge always
    /// passes `--locked`, so the project needs a `Cargo.lock` — `generate-lockfile` provides it.
    fn scaffold_project(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        let status = Command::new("cargo")
            .current_dir(dir)
            .arg("generate-lockfile")
            .status()
            .expect("cargo runs");
        assert!(status.success(), "cargo generate-lockfile failed");
    }

    /// `git init` plus a committed file, with a test identity set (git refuses to commit without
    /// one on CI runners). Returns the HEAD SHA.
    fn init_git_repo(dir: &Path) -> String {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "forge-test@example.com"]);
        git(dir, &["config", "user.name", "Forge Test"]);
        std::fs::write(dir.join("README.md"), "forge test repo\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "initial"]);
        String::from_utf8(git(dir, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    fn git_source(repo: &Path, name: &str) -> McpSource {
        McpSource {
            name: name.to_string(),
            // A `file://` URL rather than a bare Windows path: `git ls-remote` accepts both, but
            // `cargo install --git` rejects a bare `C:\...` path as an invalid URL.
            git: Some(format!(
                "file:///{}",
                repo.display().to_string().replace('\\', "/")
            )),
            path: None,
            rev: None,
            package: None,
            bin: None,
        }
    }

    fn path_source(dir: &Path, name: &str) -> McpSource {
        McpSource {
            name: name.to_string(),
            git: None,
            path: Some(dir.display().to_string()),
            rev: None,
            package: None,
            bin: None,
        }
    }

    #[test]
    fn path_source_has_no_remote_version() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_source_version(&path_source(dir.path(), "x")).unwrap(),
            None
        );
    }

    #[test]
    fn git_source_resolves_the_head_sha() {
        let repo = tempfile::tempdir().unwrap();
        let sha = init_git_repo(repo.path());

        let resolved = resolve_source_version(&git_source(repo.path(), "x")).unwrap();
        assert_eq!(resolved.as_deref(), Some(sha.as_str()));
    }

    #[test]
    fn git_source_resolves_a_named_rev() {
        let repo = tempfile::tempdir().unwrap();
        let sha = init_git_repo(repo.path());
        let mut source = git_source(repo.path(), "x");
        source.rev = Some("HEAD".into());

        assert_eq!(
            resolve_source_version(&source).unwrap().as_deref(),
            Some(sha.as_str())
        );
    }

    #[test]
    fn git_source_with_an_unknown_rev_is_an_error() {
        let repo = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let mut source = git_source(repo.path(), "x");
        source.rev = Some("no-such-ref".into());

        assert!(matches!(
            resolve_source_version(&source),
            Err(BuildError::LsRemoteStatus { .. })
        ));
    }

    #[test]
    fn sync_path_source_builds_and_verifies_the_binary() {
        let project = tempfile::tempdir().unwrap();
        scaffold_project(project.path(), "hello");
        let install = tempfile::tempdir().unwrap();
        let mut lock = LockFile::default();
        // `package` is the positional crate name for `--path` as well as `--git`.
        let mut source = path_source(project.path(), "hello");
        source.package = Some("hello".into());

        let outcome = sync_source(&source, install.path(), &mut lock, false).unwrap();
        assert_eq!(outcome, SyncOutcome::Built);
        assert!(
            managed_binary_path(install.path(), "hello").is_file(),
            "cargo install must leave the binary where the daemon looks for it"
        );
        // A path source has no remote SHA to record — the lock stays empty for it.
        assert_eq!(lock.built_sha("hello"), None);
    }

    #[test]
    fn sync_git_source_records_the_sha_and_skips_when_uptodate() {
        let repo = tempfile::tempdir().unwrap();
        scaffold_project(repo.path(), "hello");
        let sha = init_git_repo(repo.path());
        let install = tempfile::tempdir().unwrap();
        let mut lock = LockFile::default();
        let mut source = git_source(repo.path(), "hello");
        // A `package` names the crate via `cargo install`'s positional `CRATE` argument for `--git`.
        source.package = Some("hello".into());

        let first = sync_source(&source, install.path(), &mut lock, false).unwrap();
        assert_eq!(first, SyncOutcome::Built);
        assert_eq!(lock.built_sha("hello"), Some(sha.as_str()));
        assert!(managed_binary_path(install.path(), "hello").is_file());

        // Same remote SHA already built → skip the rebuild.
        let second = sync_source(&source, install.path(), &mut lock, false).unwrap();
        assert_eq!(second, SyncOutcome::UpToDate);

        // `--force` overrides the skip and rebuilds.
        let third = sync_source(&source, install.path(), &mut lock, true).unwrap();
        assert_eq!(third, SyncOutcome::Built);
    }

    #[test]
    fn sync_with_a_missing_binary_after_successful_install_is_an_error() {
        let project = tempfile::tempdir().unwrap();
        scaffold_project(project.path(), "hello");
        let install = tempfile::tempdir().unwrap();
        let mut lock = LockFile::default();

        // The source is named "mismatch" but the project builds "hello" — the expected path
        // for "mismatch" never materialises, so the install is a failure, not a silent no-op.
        let err = sync_source(
            &path_source(project.path(), "mismatch"),
            install.path(),
            &mut lock,
            false,
        )
        .unwrap_err();
        assert!(matches!(err, BuildError::MissingBinary { .. }));
    }

    #[test]
    fn cargo_install_reports_a_nonzero_exit() {
        let project = tempfile::tempdir().unwrap();
        scaffold_project(project.path(), "hello");
        let install = tempfile::tempdir().unwrap();
        let mut source = path_source(project.path(), "hello");
        // `--bin` names a binary the package does not build → cargo exits non-zero before
        // compiling anything.
        source.bin = Some("no-such-bin".into());

        let err =
            sync_source(&source, install.path(), &mut LockFile::default(), false).unwrap_err();
        assert!(matches!(err, BuildError::CargoInstallStatus { .. }));
    }

    #[test]
    #[should_panic(expected = "load_sources validates")]
    fn a_source_with_no_location_is_a_programming_error() {
        let install = tempfile::tempdir().unwrap();
        let source = McpSource {
            name: "invalid".into(),
            git: None,
            path: None,
            rev: None,
            package: None,
            bin: None,
        };
        let _ = sync_source(&source, install.path(), &mut LockFile::default(), false);
    }
}
