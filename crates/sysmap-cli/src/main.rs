//! `liberado-sysmap` — the Liberado system-map CLI.
//!
//! Builds the map (Liberado profile + topology) and either opens it in the interactive 2D window or
//! writes the JSON export. This is the thin launcher; the renderer is `liberado-sysmap-gui` and
//! the data is `liberado-sysmap` + `sysmap-core`.
//!
//! Usage:
//!   liberado-sysmap [--repo PATH] [--config-dir PATH]   open the map in a window
//!   liberado-sysmap --write-json PATH                    write the generated map JSON and exit
//!   liberado-sysmap --help                               print this help

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("liberado-sysmap: {msg}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    repo: Option<PathBuf>,
    config_dir: Option<PathBuf>,
    write_json: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        repo: None,
        config_dir: None,
        write_json: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--repo" => {
                args.repo = Some(PathBuf::from(it.next().ok_or("--repo requires a path")?));
            }
            "--config-dir" => {
                args.config_dir = Some(PathBuf::from(
                    it.next().ok_or("--config-dir requires a path")?,
                ));
            }
            "--write-json" => {
                args.write_json = Some(PathBuf::from(
                    it.next().ok_or("--write-json requires a path")?,
                ));
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other} (see --help)")),
        }
    }
    Ok(args)
}

fn print_help() {
    println!(
        "liberado-sysmap — interactive system map of Liberado\n\n\
         USAGE:\n    liberado-sysmap [OPTIONS]\n\n\
         OPTIONS:\n    \
         --repo <PATH>        repository root to scan (default: walk up from cwd)\n    \
         --config-dir <PATH>  config dir holding topology.toml (default: LIBERADO_CONFIG_DIR,\n    \
                              then the platform config dir's liberado/ subfolder)\n    \
         --write-json <PATH>  write the generated map as JSON and exit (headless, no window)\n    \
         --help               print this help\n\n\
         The map is regenerated from cargo metadata and an optional topology.toml on every\n\
         launch — nothing is hand-drawn, so dependency changes appear on the next run."
    );
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    let repo = match args.repo {
        Some(p) => p,
        None => liberado_sysmap::repository_root()?,
    };
    let config_dir = liberado_sysmap::resolve_config_dir(args.config_dir.as_deref());

    let map = liberado_sysmap::build(&repo, config_dir.as_deref()).map_err(|e| e.to_string())?;

    if let Some(out) = args.write_json {
        let json = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
        std::fs::write(&out, json).map_err(|e| format!("writing {}: {e}", out.display()))?;
        println!(
            "Wrote {} ({} nodes, {} edges)",
            out.display(),
            map.nodes.len(),
            map.edges.len()
        );
        return Ok(());
    }

    liberado_sysmap_gui::launch(map, repo)
}
