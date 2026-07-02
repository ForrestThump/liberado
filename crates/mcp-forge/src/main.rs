//! `liberado-mcp-forge` — builds and installs Liberado MCP servers from git URLs via
//! `cargo install --git`, so `topology.toml`'s `McpTransport::Managed` entries have a binary to
//! find at connect-time. See `ARCHITECTURE.md` for the design.
//!
//! Usage:
//!   liberado-mcp-forge sync [--force] [--only <name>]
//!
//! Reads `mcp-sources.toml` from the same config directory the daemon resolves
//! (`LIBERADO_CONFIG_DIR` or the platform default). Installs into `LIBERADO_MCP_INSTALL_DIR`
//! (or its platform-default equivalent) — see `liberado_bootstrap::mcp_install_dir`.

mod build;
mod lock;
mod sources;

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
    let Some(config_dir) = liberado_bootstrap::config_dir() else {
        eprintln!("no config directory found (set LIBERADO_CONFIG_DIR)");
        return ExitCode::FAILURE;
    };

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

    let install_dir = liberado_bootstrap::mcp_install_dir();
    let mut lock = LockFile::load(&install_dir);

    let mut failed = false;
    for source in &sources {
        match build::sync_source(source, &install_dir, &mut lock, force) {
            Ok(build::SyncOutcome::UpToDate) => println!("[{}] up to date", source.name),
            Ok(build::SyncOutcome::Built) => println!("[{}] built", source.name),
            Err(e) => {
                eprintln!("[{}] FAILED: {e}", source.name);
                failed = true;
            }
        }
    }

    if let Err(e) = lock.save(&install_dir) {
        eprintln!("warning: failed to save lockfile: {e}");
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
