//! File-level structural health ratchet backed by Mozilla rust-code-analysis.

mod analysis;

use liberado_common::process::std_command;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const TOOL: &str = "rust-code-analysis-cli";
const TOOL_VERSION: &str = "0.0.25";
const CONFIG_FILE: &str = "module-health.toml";
const BASELINE_FILE: &str = "module-health-baseline.json";
const CURRENT_FILE: &str = ".liberado/module-health-current.json";
const ANALYSIS_DIR: &str = ".liberado/rust-code-analysis";

#[derive(Debug, Deserialize)]
struct Config {
    thresholds: Thresholds,
    #[serde(default)]
    waiver: Vec<Waiver>,
}

#[derive(Debug, Deserialize)]
struct Thresholds {
    ploc_review: u64,
    ploc_new: u64,
    lloc_review: u64,
    lloc_new: u64,
    functions_review: u64,
    functions_new: u64,
    cyclomatic_review: u64,
    cyclomatic_new: u64,
}

#[derive(Debug, Deserialize)]
struct Waiver {
    path: String,
    metric: Metric,
    ceiling: u64,
    reason: String,
    reviewed_on: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum Metric {
    Ploc,
    Lloc,
    Functions,
    Cyclomatic,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct FileMetrics {
    ploc: u64,
    lloc: u64,
    functions: u64,
    cyclomatic: u64,
}

type Report = BTreeMap<String, FileMetrics>;

pub fn check(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(root)?;
    let current = analysis::analyze(root)?;
    write_report(&root.join(CURRENT_FILE), &current)?;
    let baseline = read_report(&root.join(BASELINE_FILE))?;
    compare(&config, &baseline, &current)?;
    eprintln!(
        "[module health] ok: {} production Rust files",
        current.len()
    );
    Ok(())
}

fn read_report(path: &Path) -> Result<Report, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

/// The current report for a ratchet: compare against the existing baseline when there is one,
/// otherwise produce an initial report from a fresh analysis. Split from [`ratchet`] so the
/// driver stays under the complexity ceiling; the analysis half runs the real tool and is
/// covered by `just ci` itself.
fn current_report(root: &Path) -> Result<Report, Box<dyn std::error::Error>> {
    if root.join(BASELINE_FILE).is_file() {
        existing_report(root)
    } else {
        initial_report(root)
    }
}

fn existing_report(root: &Path) -> Result<Report, Box<dyn std::error::Error>> {
    check(root)?;
    read_report(&root.join(CURRENT_FILE))
}

fn initial_report(root: &Path) -> Result<Report, Box<dyn std::error::Error>> {
    load_config(root)?;
    let report = analysis::analyze(root)?;
    write_report(&root.join(CURRENT_FILE), &report)?;
    eprintln!("[module health] creating initial baseline");
    Ok(report)
}

pub fn ratchet(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let current = current_report(root)?;
    write_report(&root.join(BASELINE_FILE), &current)?;
    eprintln!("[module health] ratcheted {BASELINE_FILE}");
    Ok(())
}

fn load_config(root: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let config: Config = toml::from_str(&std::fs::read_to_string(root.join(CONFIG_FILE))?)?;
    for metric in [
        Metric::Ploc,
        Metric::Lloc,
        Metric::Functions,
        Metric::Cyclomatic,
    ] {
        let (review, new_limit) = limits(&config.thresholds, metric);
        if review >= new_limit {
            return Err(format!(
                "{metric:?} review boundary {review} must be below new-file ceiling {new_limit}"
            )
            .into());
        }
    }
    validate_waivers(root, &config)?;
    Ok(config)
}

fn validate_waivers(root: &Path, config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut seen = BTreeSet::new();
    for waiver in &config.waiver {
        if waiver.reason.trim().is_empty() || waiver.reviewed_on.trim().is_empty() {
            return Err(format!(
                "waiver for {} must include reason and reviewed_on",
                waiver.path
            )
            .into());
        }
        if !root.join(&waiver.path).is_file() {
            return Err(
                format!("stale module-health waiver: {} does not exist", waiver.path).into(),
            );
        }
        if !seen.insert((waiver.path.clone(), waiver.metric)) {
            return Err(
                format!("duplicate waiver for {} / {:?}", waiver.path, waiver.metric).into(),
            );
        }
        let review = limits(&config.thresholds, waiver.metric).0;
        if waiver.ceiling <= review {
            return Err(format!("waiver for {} / {:?} is unnecessary: ceiling {} is not above review boundary {review}", waiver.path, waiver.metric, waiver.ceiling).into());
        }
    }
    Ok(())
}

fn verify_tool() -> Result<(), Box<dyn std::error::Error>> {
    let mut command = std_command(TOOL);
    let output = command.arg("--version").output().map_err(|_| {
        format!("missing {TOOL} {TOOL_VERSION}; install with `cargo install {TOOL} --version {TOOL_VERSION} --locked`")
    })?;
    let version = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !version.split_whitespace().any(|part| part == TOOL_VERSION) {
        return Err(format!("expected {TOOL} {TOOL_VERSION}, got `{}`", version.trim()).into());
    }
    Ok(())
}

fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_json(&path, out)?;
        } else if path.extension().and_then(|v| v.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

fn is_production_source(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    parts.len() >= 4 && parts[0] == "crates" && parts[2] == "src" && path.ends_with(".rs")
}

fn exact(value: f64, path: &str, metric: &str) -> Result<u64, Box<dyn std::error::Error>> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return Err(format!("non-integer {metric} value {value} for {path}").into());
    }
    Ok(value as u64)
}
fn compare(
    config: &Config,
    baseline: &Report,
    current: &Report,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut failures = Vec::new();
    for (path, now) in current {
        for metric in [
            Metric::Ploc,
            Metric::Lloc,
            Metric::Functions,
            Metric::Cyclomatic,
        ] {
            let current_value = value(now, metric);
            let (review, new_limit) = limits(&config.thresholds, metric);
            let waiver = config
                .waiver
                .iter()
                .find(|w| w.path == *path && w.metric == metric);
            if let Some(failure) = metric_failure(
                path,
                metric,
                current_value,
                baseline.get(path),
                waiver,
                review,
                new_limit,
            ) {
                failures.push(failure);
            }
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    failures.sort();
    Err(format!("module-health regression:\n{}\nSplit the file or add a metric-specific reviewed waiver; do not raise the baseline.", failures.join("\n")).into())
}

/// One metric of one file against the baseline, the boundaries, and any
/// waiver. A waiver governs the file outright: within its ceiling the file
/// passes even where the baseline-regression rule would fail it.
#[allow(clippy::too_many_arguments)]
fn metric_failure(
    path: &str,
    metric: Metric,
    current_value: u64,
    old: Option<&FileMetrics>,
    waiver: Option<&Waiver>,
    review: u64,
    new_limit: u64,
) -> Option<String> {
    let limit = waiver.map_or(new_limit, |w| w.ceiling);
    match old {
        None if current_value > limit => Some(format!(
            "{path}: new-file {metric:?} {current_value} > {limit}"
        )),
        Some(_) if waiver.is_some() && current_value > limit => Some(format!(
            "{path}: waived {metric:?} {current_value} > ceiling {limit}"
        )),
        // Without a waiver the file may not grow past the review boundary.
        Some(old)
            if waiver.is_none() && current_value > value(old, metric) && current_value > review =>
        {
            Some(format!(
                "{path}: {metric:?} regressed {} -> {current_value} (review boundary {review})",
                value(old, metric)
            ))
        }
        _ => None,
    }
}

fn value(metrics: &FileMetrics, metric: Metric) -> u64 {
    match metric {
        Metric::Ploc => metrics.ploc,
        Metric::Lloc => metrics.lloc,
        Metric::Functions => metrics.functions,
        Metric::Cyclomatic => metrics.cyclomatic,
    }
}

fn limits(t: &Thresholds, metric: Metric) -> (u64, u64) {
    match metric {
        Metric::Ploc => (t.ploc_review, t.ploc_new),
        Metric::Lloc => (t.lloc_review, t.lloc_new),
        Metric::Functions => (t.functions_review, t.functions_new),
        Metric::Cyclomatic => (t.cyclomatic_review, t.cyclomatic_new),
    }
}

fn write_report(path: &Path, report: &Report) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut json = serde_json::to_string_pretty(report)?;
    json.push('\n');
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn config() -> Config {
        Config {
            thresholds: Thresholds {
                ploc_review: 100,
                ploc_new: 150,
                lloc_review: 70,
                lloc_new: 100,
                functions_review: 10,
                functions_new: 15,
                cyclomatic_review: 20,
                cyclomatic_new: 30,
            },
            waiver: vec![],
        }
    }

    #[test]
    fn healthy_growth_is_allowed_but_over_boundary_regression_fails() {
        let old = FileMetrics {
            ploc: 80,
            ..Default::default()
        };
        let healthy = FileMetrics {
            ploc: 90,
            ..Default::default()
        };
        let regressed = FileMetrics {
            ploc: 101,
            ..Default::default()
        };
        let baseline = BTreeMap::from([("crates/a/src/lib.rs".into(), old.clone())]);
        assert!(
            compare(
                &config(),
                &baseline,
                &BTreeMap::from([("crates/a/src/lib.rs".into(), healthy)])
            )
            .is_ok()
        );
        assert!(
            compare(
                &config(),
                &baseline,
                &BTreeMap::from([("crates/a/src/lib.rs".into(), regressed)])
            )
            .is_err()
        );
    }

    #[test]
    fn new_file_has_a_hard_ceiling() {
        let current = BTreeMap::from([(
            "crates/a/src/new.rs".into(),
            FileMetrics {
                functions: 16,
                ..Default::default()
            },
        )]);
        assert!(compare(&config(), &Report::new(), &current).is_err());
    }

    #[test]
    fn waiver_applies_only_to_its_metric_and_ceiling() {
        let mut cfg = config();
        cfg.waiver.push(Waiver {
            path: "crates/a/src/table.rs".into(),
            metric: Metric::Ploc,
            ceiling: 300,
            reason: "declarative table".into(),
            reviewed_on: "2026-08-21".into(),
        });
        let current = BTreeMap::from([(
            "crates/a/src/table.rs".into(),
            FileMetrics {
                ploc: 250,
                functions: 16,
                ..Default::default()
            },
        )]);
        let error = compare(&cfg, &Report::new(), &current)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Functions"));
        assert!(!error.contains("Ploc"));
    }

    #[test]
    fn waiver_exempts_a_growing_existing_file_within_its_ceiling() {
        let mut cfg = config();
        cfg.waiver.push(Waiver {
            path: "crates/a/src/big.rs".into(),
            metric: Metric::Ploc,
            ceiling: 1200,
            reason: "single authority".into(),
            reviewed_on: "2026-08-24".into(),
        });
        let baseline = BTreeMap::from([(
            "crates/a/src/big.rs".into(),
            FileMetrics {
                ploc: 1100,
                ..Default::default()
            },
        )]);
        // 1150 is over the review boundary (100) and over baseline, but under
        // the waiver ceiling: the waiver governs, so no failure.
        let grown = FileMetrics {
            ploc: 1150,
            ..Default::default()
        };
        assert!(
            compare(
                &cfg,
                &baseline,
                &BTreeMap::from([("crates/a/src/big.rs".into(), grown.clone())])
            )
            .is_ok()
        );
        // Past the ceiling it fails again, naming the ceiling.
        let over = FileMetrics {
            ploc: 1250,
            ..Default::default()
        };
        let error = compare(
            &cfg,
            &baseline,
            &BTreeMap::from([("crates/a/src/big.rs".into(), over)]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("ceiling 1200"), "{error}");
        // Without the waiver the same growth still regresses.
        assert!(
            compare(
                &config(),
                &baseline,
                &BTreeMap::from([("crates/a/src/big.rs".into(), grown)])
            )
            .is_err()
        );
    }

    #[test]
    fn collect_json_recurses_and_keeps_only_json_files() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("nested/deeper")).unwrap();
        std::fs::write(root.path().join("root.json"), "{}").unwrap();
        std::fs::write(root.path().join("nested/deeper/report.json"), "{}").unwrap();
        std::fs::write(root.path().join("nested/notes.txt"), "ignore").unwrap();
        let mut paths = Vec::new();
        collect_json(root.path(), &mut paths).unwrap();
        paths.sort();
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|path| path.extension().unwrap() == "json"));
    }

    #[test]
    fn waiver_validation_accepts_valid_and_rejects_invalid_metadata() {
        let root = tempdir().unwrap();
        let source = "crates/demo/src/lib.rs";
        std::fs::create_dir_all(root.path().join(source).parent().unwrap()).unwrap();
        std::fs::write(root.path().join(source), "fn demo() {}\n").unwrap();
        let mut cfg = config();
        cfg.waiver.push(Waiver {
            path: source.into(),
            metric: Metric::Ploc,
            ceiling: 200,
            reason: "generated table".into(),
            reviewed_on: "2026-08-31".into(),
        });
        validate_waivers(root.path(), &cfg).unwrap();

        cfg.waiver[0].reason.clear();
        assert!(
            validate_waivers(root.path(), &cfg)
                .unwrap_err()
                .to_string()
                .contains("reason")
        );
    }
}
