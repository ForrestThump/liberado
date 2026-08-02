//! `liberado-conformance` — Tier 3 live path runner (hand-run on the homelab box for v1).

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use chrono::Utc;
use liberado_conformance::client::DaemonClient;
use liberado_conformance::config::ConformanceConfig;
use liberado_conformance::paths::run_path;
use liberado_conformance::report::write_vault_report;
use liberado_conformance::result::{PathId, PathStatus, RunReport};

fn usage() -> ! {
    eprintln!(
        "usage: liberado-conformance --config <path/to/conformance.toml> [--path p1a,p1b,...] [--advisory-counts]"
    );
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let mut config_path: Option<PathBuf> = None;
    let mut path_filter: Option<Vec<PathId>> = None;
    let mut advisory_counts = false;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" | "-c" => {
                config_path = Some(PathBuf::from(args.next().unwrap_or_else(|| usage())));
            }
            "--path" | "--paths" => {
                let list = args.next().unwrap_or_else(|| usage());
                let mut ids = Vec::new();
                for part in list.split(',') {
                    match PathId::parse(part) {
                        Some(id) => ids.push(id),
                        None => {
                            eprintln!("unknown path: {part}");
                            usage();
                        }
                    }
                }
                path_filter = Some(ids);
            }
            "--advisory-counts" => advisory_counts = true,
            "--help" | "-h" => usage(),
            other => {
                eprintln!("unknown flag: {other}");
                usage();
            }
        }
    }

    let config_path = config_path.unwrap_or_else(|| {
        eprintln!("--config is required");
        usage();
    });

    let mut cfg = match ConformanceConfig::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::from(2);
        }
    };
    if advisory_counts {
        cfg.advisory_counts = true;
    }

    let paths = path_filter.unwrap_or_else(|| {
        if cfg.paths.is_empty() {
            PathId::all_default()
        } else {
            cfg.paths
                .iter()
                .filter_map(|s| PathId::parse(s))
                .collect()
        }
    });

    let client = match DaemonClient::new(&cfg.base_url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("client error: {e}");
            return ExitCode::from(2);
        }
    };

    let started_at = Utc::now();
    eprintln!(
        "liberado-conformance: base_url={} paths={:?} budget={}s",
        cfg.base_url,
        paths.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        cfg.budget_secs
    );

    let deadline = Instant::now() + cfg.budget();
    let mut results = Vec::new();
    for id in paths {
        eprintln!("… running {}", id.as_str());
        let r = run_path(id, &client, &cfg, deadline).await;
        eprintln!(
            "  {} → {:?} ({} ms)",
            r.path, r.status, r.duration_ms
        );
        // stdout: one JSON object per path
        match serde_json::to_string(&r) {
            Ok(line) => println!("{line}"),
            Err(e) => eprintln!("serialize result: {e}"),
        }
        results.push(r);
    }

    let finished_at = Utc::now();
    let overall = RunReport::compute_overall(&results, cfg.advisory_counts);
    let report = RunReport {
        started_at: started_at.to_rfc3339(),
        finished_at: finished_at.to_rfc3339(),
        overall,
        base_url: cfg.base_url.clone(),
        results,
    };

    match write_vault_report(&cfg.vault_path, &report) {
        Ok(rel) => eprintln!("vault report: {}", rel.display()),
        Err(e) => eprintln!("vault report failed: {e}"),
    }

    eprintln!("overall: {overall:?}");
    if overall == PathStatus::Fail {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
