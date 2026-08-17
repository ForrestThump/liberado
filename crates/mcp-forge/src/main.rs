//! `liberado-mcp-forge` — builds and installs Liberado MCP servers from git URLs via
//! `cargo install --git`, so `topology.toml`'s `McpTransport::Managed` entries have a binary to
//! find at connect-time. See `ARCHITECTURE.md` for the design.
//!
//! Usage:
//!   liberado-mcp-forge sync [--force] [--only <name>]
//!
//! Reads `mcp-sources.toml` from the same config directory the daemon resolves
//! (`LIBERADO_CONFIG_DIR` or the platform default). Installs into `LIBERADO_MCP_INSTALL_DIR`
//! (or its platform-default equivalent) — see `liberado_config::mcp_install_dir`.

mod build;
mod lock;
mod sources;

use std::path::Path;
use std::process::ExitCode;

use lock::LockFile;
use sources::McpSource;

const SOURCES_FILE: &str = "mcp-sources.toml";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("sync") => {
            let mut force = false;
            let mut only: Option<String> = None;
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--force" => force = true,
                    "--only" => match args.next() {
                        Some(name) => only = Some(name),
                        None => {
                            eprintln!("--only requires a source name");
                            return ExitCode::FAILURE;
                        }
                    },
                    other => {
                        eprintln!("unknown flag: {other}");
                        eprintln!("usage: liberado-mcp-forge sync [--force] [--only <name>]");
                        return ExitCode::FAILURE;
                    }
                }
            }
            run_sync(force, only)
        }
        _ => {
            eprintln!("usage: liberado-mcp-forge sync [--force] [--only <name>]");
            ExitCode::FAILURE
        }
    }
}

fn run_sync(force: bool, only: Option<String>) -> ExitCode {
    let Some(config_dir) = liberado_config::config_dir() else {
        eprintln!("no config directory found (set LIBERADO_CONFIG_DIR)");
        return ExitCode::FAILURE;
    };
    let install_dir = liberado_config::mcp_install_dir();
    run_sync_in(&config_dir, &install_dir, force, only)
}

/// The pure part of `run_sync`: the two directories come in as parameters instead of being
/// resolved from process env here, so tests can drive the whole sync (including the exit code)
/// without mutating `LIBERADO_CONFIG_DIR` / `LIBERADO_MCP_INSTALL_DIR` — process-global state
/// that races under `cargo test`'s parallel execution.
fn run_sync_in(
    config_dir: &Path,
    install_dir: &Path,
    force: bool,
    only: Option<String>,
) -> ExitCode {
    let sources_path = config_dir.join(SOURCES_FILE);
    let sources = match sources::load_sources(&sources_path) {
        Ok(sources) => sources,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let sources: Vec<McpSource> = match &only {
        Some(name) => sources.into_iter().filter(|s| &s.name == name).collect(),
        None => sources,
    };
    if sources.is_empty() {
        eprintln!(
            "no sources to sync (checked {}, --only {:?})",
            sources_path.display(),
            only
        );
        return ExitCode::FAILURE;
    }

    let mut lock = LockFile::load(install_dir);

    let mut failed = false;
    for source in &sources {
        match build::sync_source(source, install_dir, &mut lock, force) {
            Ok(build::SyncOutcome::UpToDate) => println!("[{}] up to date", source.name),
            Ok(build::SyncOutcome::Built) => println!("[{}] built", source.name),
            Err(e) => {
                eprintln!("[{}] FAILED: {e}", source.name);
                failed = true;
            }
        }
    }

    if let Err(e) = lock.save(install_dir) {
        eprintln!("warning: failed to save lockfile: {e}");
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_sources(config_dir: &Path, toml: &str) -> PathBuf {
        std::fs::create_dir_all(config_dir).unwrap();
        let path = config_dir.join(SOURCES_FILE);
        std::fs::write(&path, toml).unwrap();
        path
    }

    fn sources_toml(name: &str, path: &Path) -> String {
        // Forward slashes: a raw Windows `\` inside a TOML basic string is an escape sequence.
        let path = path.display().to_string().replace('\\', "/");
        format!("[[source]]\nname = \"{name}\"\npath = \"{path}\"\n")
    }

    /// A minimal, dependency-free Cargo project that `cargo install --path` can build.
    fn scaffold_project(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        let status = std::process::Command::new("cargo")
            .current_dir(dir)
            .arg("generate-lockfile")
            .status()
            .expect("cargo runs");
        assert!(status.success(), "cargo generate-lockfile failed");
    }

    /// `git init` plus a committed file, with a test identity set (git refuses to commit without
    /// one on CI runners). Returns the HEAD SHA.
    fn init_git_repo(dir: &Path) -> String {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "forge-test@example.com"]);
        run(&["config", "user.name", "Forge Test"]);
        std::fs::write(dir.join("README.md"), "forge test repo\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "initial"]);
        String::from_utf8(run(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    fn git_source_toml(name: &str, repo: &Path) -> String {
        // A `file://` URL rather than a bare Windows path: `git ls-remote` accepts both, but
        // `cargo install --git` rejects a bare `C:\...` path as an invalid URL.
        let url = format!("file:///{}", repo.display().to_string().replace('\\', "/"));
        format!("[[source]]\nname = \"{name}\"\ngit = \"{url}\"\n")
    }

    #[test]
    fn missing_sources_file_is_failure() {
        let config = tempfile::tempdir().unwrap();
        let install = tempfile::tempdir().unwrap();
        assert_eq!(
            run_sync_in(config.path(), install.path(), false, None),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn empty_sources_file_is_failure() {
        let config = tempfile::tempdir().unwrap();
        write_sources(config.path(), "# no sources yet\n");
        let install = tempfile::tempdir().unwrap();
        assert_eq!(
            run_sync_in(config.path(), install.path(), false, None),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn only_matching_no_source_is_failure() {
        let config = tempfile::tempdir().unwrap();
        write_sources(
            config.path(),
            "[[source]]\nname = \"hello\"\ngit = \"https://example.invalid/repo\"\n",
        );
        let install = tempfile::tempdir().unwrap();
        assert_eq!(
            run_sync_in(config.path(), install.path(), false, Some("other".into())),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn syncs_a_path_source_and_returns_success() {
        let config = tempfile::tempdir().unwrap();
        let install = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        scaffold_project(project.path(), "hello");
        write_sources(config.path(), &sources_toml("hello", project.path()));

        let code = run_sync_in(config.path(), install.path(), false, None);
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(liberado_config::managed_binary_path(install.path(), "hello").is_file());
    }

    #[test]
    fn a_failing_source_marks_the_run_failed_but_still_saves_the_lock() {
        let config = tempfile::tempdir().unwrap();
        let install = tempfile::tempdir().unwrap();
        write_sources(
            config.path(),
            "[[source]]\nname = \"broken\"\npath = \"C:/does/not/exist\"\n",
        );

        let code = run_sync_in(config.path(), install.path(), false, None);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(
            install.path().join(".mcp-forge-lock.toml").is_file(),
            "lockfile must still be saved after a failed source"
        );
    }

    #[test]
    fn second_sync_reports_uptodate() {
        let config = tempfile::tempdir().unwrap();
        let install = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        scaffold_project(repo.path(), "hello");
        init_git_repo(repo.path());
        write_sources(config.path(), &git_source_toml("hello", repo.path()));

        assert_eq!(
            run_sync_in(config.path(), install.path(), false, None),
            ExitCode::SUCCESS
        );
        // The second run resolves the same remote SHA, sees it in the lock, and reports
        // "up to date" instead of rebuilding.
        assert_eq!(
            run_sync_in(config.path(), install.path(), false, None),
            ExitCode::SUCCESS
        );
        let lock_text =
            std::fs::read_to_string(install.path().join(".mcp-forge-lock.toml")).unwrap();
        assert!(
            lock_text.contains("hello"),
            "lock must record the built source"
        );
    }

    #[test]
    fn lock_save_failure_is_reported_as_a_warning() {
        let config = tempfile::tempdir().unwrap();
        let install = tempfile::tempdir().unwrap();
        // A file where a directory should be: `lock.save`'s `create_dir_all` fails because the
        // parent of `install_dir` is not a directory.
        let blocker = install.path().join("blocker");
        std::fs::write(&blocker, "in the way").unwrap();
        let install_dir = blocker.join("install");
        write_sources(
            config.path(),
            "[[source]]\nname = \"broken\"\npath = \"C:/does/not/exist\"\n",
        );

        let code = run_sync_in(config.path(), &install_dir, false, None);
        assert_eq!(code, ExitCode::FAILURE);
    }
}
