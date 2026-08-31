//! Production unwraps classification and regression ratchet.

mod classifier;

pub use classifier::{
    Classification, FileUnwrapMetrics, Report, SummaryMetrics, UnwrapOccurrence, analyze_tree,
};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

const CONFIG_FILE: &str = "unwrap-classification.toml";
const BASELINE_FILE: &str = "unwrap-classification-baseline.json";
const CURRENT_FILE: &str = ".liberado/unwrap-classification-current.json";

#[derive(Debug, Deserialize)]
struct Config {
    thresholds: Thresholds,
    #[serde(default)]
    waiver: Vec<Waiver>,
}

#[derive(Debug, Deserialize)]
struct Thresholds {
    process_fatal_new: usize,
    local_failure_new: usize,
}

#[derive(Debug, Deserialize)]
struct Waiver {
    path: String,
    metric: Metric,
    ceiling: usize,
    reason: String,
    reviewed_on: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum Metric {
    ProvenInvariant,
    LocalFailure,
    ProcessFatal,
    Total,
}

pub fn check(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(root)?;
    let current = analyze_tree(root)?;
    write_report(&root.join(CURRENT_FILE), &current)?;

    let baseline_path = root.join(BASELINE_FILE);
    if baseline_path.is_file() {
        let baseline: Report = serde_json::from_slice(&std::fs::read(&baseline_path)?)?;
        compare(&config, &baseline, &current)?;
    } else {
        validate_waivers(&config, &current)?;
    }

    eprintln!(
        "[unwraps] ok: {} unwraps scanned across {} files ({} proven_invariant, {} local_failure, {} process_fatal)",
        current.summary.total_unwraps,
        current.summary.files_scanned,
        current.summary.proven_invariant,
        current.summary.local_failure,
        current.summary.process_fatal
    );
    Ok(())
}

pub fn ratchet(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(root)?;
    let current = analyze_tree(root)?;
    write_report(&root.join(CURRENT_FILE), &current)?;
    validate_waivers(&config, &current)?;
    write_report(&root.join(BASELINE_FILE), &current)?;
    eprintln!("[unwraps] ratcheted {BASELINE_FILE}");
    Ok(())
}

fn load_config(root: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let path = root.join(CONFIG_FILE);
    if !path.is_file() {
        return Err(format!("missing {CONFIG_FILE}").into());
    }
    let config: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
    let mut seen = BTreeSet::new();
    for waiver in &config.waiver {
        if waiver.reason.trim().is_empty() || waiver.reviewed_on.trim().is_empty() {
            return Err(format!(
                "unwrap waiver for {} / {:?} needs reason and reviewed_on",
                waiver.path, waiver.metric
            )
            .into());
        }
        if !root.join(&waiver.path).is_file() {
            return Err(format!("stale unwrap waiver: {}", waiver.path).into());
        }
        if !seen.insert((waiver.path.clone(), waiver.metric)) {
            return Err(format!(
                "duplicate unwrap waiver for {} / {:?}",
                waiver.path, waiver.metric
            )
            .into());
        }
    }
    Ok(config)
}

fn write_report(dest: &Path, report: &Report) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(report)?;
    std::fs::write(dest, json)?;
    Ok(())
}

fn compare(
    config: &Config,
    baseline: &Report,
    current: &Report,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_waivers(config, current)?;

    // Per-file comparison against baseline
    for (file, curr_metrics) in &current.files {
        let base_metrics = baseline.files.get(file);
        let base_fatal = base_metrics.map(|b| b.process_fatal).unwrap_or(0);
        let base_local = base_metrics.map(|b| b.local_failure).unwrap_or(0);

        let fatal_waiver = config
            .waiver
            .iter()
            .find(|w| w.path == *file && w.metric == Metric::ProcessFatal)
            .map(|w| w.ceiling);

        let local_waiver = config
            .waiver
            .iter()
            .find(|w| w.path == *file && w.metric == Metric::LocalFailure)
            .map(|w| w.ceiling);

        let fatal_ceiling =
            fatal_waiver.unwrap_or(base_fatal.max(config.thresholds.process_fatal_new));
        let local_ceiling =
            local_waiver.unwrap_or(base_local.max(config.thresholds.local_failure_new));

        if curr_metrics.process_fatal > fatal_ceiling {
            return Err(format!(
                "[unwraps] {file} regressed process_fatal unwraps: {} > ceiling {}",
                curr_metrics.process_fatal, fatal_ceiling
            )
            .into());
        }

        if curr_metrics.local_failure > local_ceiling {
            return Err(format!(
                "[unwraps] {file} regressed local_failure unwraps: {} > ceiling {}",
                curr_metrics.local_failure, local_ceiling
            )
            .into());
        }
    }

    Ok(())
}

fn validate_waivers(config: &Config, current: &Report) -> Result<(), Box<dyn std::error::Error>> {
    for waiver in &config.waiver {
        let file_metrics = current.files.get(&waiver.path);
        let count = match waiver.metric {
            Metric::ProcessFatal => file_metrics.map(|m| m.process_fatal).unwrap_or(0),
            Metric::LocalFailure => file_metrics.map(|m| m.local_failure).unwrap_or(0),
            Metric::ProvenInvariant => file_metrics.map(|m| m.proven_invariant).unwrap_or(0),
            Metric::Total => file_metrics.map(|m| m.total).unwrap_or(0),
        };
        if count > waiver.ceiling {
            return Err(format!(
                "[unwraps] {} exceeds {:?} waiver ceiling: {} > {}",
                waiver.path, waiver.metric, count, waiver.ceiling
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const CLEAN_CONFIG: &str = r#"
[thresholds]
process_fatal_new = 0
local_failure_new = 0
"#;

    fn write_clean_workspace(root: &Path) {
        std::fs::write(root.join(CONFIG_FILE), CLEAN_CONFIG).unwrap();
        let source = root.join("crates/sample/src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("lib.rs"), "pub fn clean() {}\n").unwrap();
    }

    #[test]
    fn check_accepts_a_baseline_free_clean_tree_and_writes_evidence() {
        let root = tempdir().unwrap();
        write_clean_workspace(root.path());

        check(root.path()).unwrap();

        let current = root.path().join(CURRENT_FILE);
        assert!(current.is_file());
        let report: Report = serde_json::from_slice(&std::fs::read(current).unwrap()).unwrap();
        assert_eq!(report.summary.files_scanned, 1);
        assert_eq!(report.summary.total_unwraps, 0);
    }

    #[test]
    fn load_config_rejects_missing_stale_incomplete_and_duplicate_waivers() {
        let root = tempdir().unwrap();
        assert!(
            load_config(root.path())
                .unwrap_err()
                .to_string()
                .contains("missing")
        );

        std::fs::write(
            root.path().join(CONFIG_FILE),
            format!(
                "{CLEAN_CONFIG}\n[[waiver]]\npath = \"missing.rs\"\nmetric = \"total\"\nceiling = 1\nreason = \"\"\nreviewed_on = \"\"\n"
            ),
        )
        .unwrap();
        assert!(
            load_config(root.path())
                .unwrap_err()
                .to_string()
                .contains("needs reason")
        );

        std::fs::write(
            root.path().join(CONFIG_FILE),
            format!(
                "{CLEAN_CONFIG}\n[[waiver]]\npath = \"missing.rs\"\nmetric = \"total\"\nceiling = 1\nreason = \"legacy\"\nreviewed_on = \"2026-08-31\"\n"
            ),
        )
        .unwrap();
        assert!(
            load_config(root.path())
                .unwrap_err()
                .to_string()
                .contains("stale")
        );

        std::fs::write(root.path().join("tracked.rs"), "fn tracked() {}\n").unwrap();
        let waiver = "\n[[waiver]]\npath = \"tracked.rs\"\nmetric = \"total\"\nceiling = 1\nreason = \"legacy\"\nreviewed_on = \"2026-08-31\"\n";
        std::fs::write(
            root.path().join(CONFIG_FILE),
            format!("{CLEAN_CONFIG}{waiver}{waiver}"),
        )
        .unwrap();
        assert!(
            load_config(root.path())
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[test]
    fn waiver_validation_checks_each_metric_and_its_ceiling() {
        let mut report = Report::default();
        report.files.insert(
            "tracked.rs".into(),
            FileUnwrapMetrics {
                proven_invariant: 1,
                local_failure: 2,
                process_fatal: 3,
                total: 6,
                ..Default::default()
            },
        );
        for (metric, ceiling) in [
            (Metric::ProvenInvariant, 0),
            (Metric::LocalFailure, 1),
            (Metric::ProcessFatal, 2),
            (Metric::Total, 5),
        ] {
            let config = Config {
                thresholds: Thresholds {
                    process_fatal_new: 0,
                    local_failure_new: 0,
                },
                waiver: vec![Waiver {
                    path: "tracked.rs".into(),
                    metric,
                    ceiling,
                    reason: "test".into(),
                    reviewed_on: "2026-08-31".into(),
                }],
            };
            assert!(validate_waivers(&config, &report).is_err());
        }
    }
}
