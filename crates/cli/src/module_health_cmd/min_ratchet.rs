//! Write file-health baselines without losing better existing metrics.

use super::{
    BASELINE_FILE, CURRENT_FILE, FileMetrics, Report, analysis, check, load_config, read_report,
    write_report,
};
use std::path::Path;

pub(super) fn write(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let current = current_report(root)?;
    let ratcheted = if root.join(BASELINE_FILE).is_file() {
        ratcheted_report(&read_report(&root.join(BASELINE_FILE))?, &current)
    } else {
        current
    };
    write_report(&root.join(BASELINE_FILE), &ratcheted)?;
    eprintln!("[module health] ratcheted {BASELINE_FILE}");
    Ok(())
}

/// Compare against the existing baseline when there is one. Otherwise, produce an initial
/// report from a fresh analysis.
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

pub(super) fn ratcheted_report(baseline: &Report, current: &Report) -> Report {
    current
        .iter()
        .map(|(path, now)| {
            let metrics = baseline.get(path).map_or_else(
                || now.clone(),
                |old| FileMetrics {
                    ploc: old.ploc.min(now.ploc),
                    lloc: old.lloc.min(now.lloc),
                    functions: old.functions.min(now.functions),
                    cyclomatic: old.cyclomatic.min(now.cyclomatic),
                },
            );
            (path.clone(), metrics)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{FileMetrics, Report, ratcheted_report};
    use std::collections::BTreeMap;

    #[test]
    fn ratchet_does_not_raise_existing_file_metrics() {
        let baseline = BTreeMap::from([(
            "crates/a/src/lib.rs".into(),
            FileMetrics {
                ploc: 80,
                lloc: 50,
                functions: 8,
                cyclomatic: 15,
            },
        )]);
        let current = BTreeMap::from([(
            "crates/a/src/lib.rs".into(),
            FileMetrics {
                ploc: 90,
                lloc: 45,
                functions: 9,
                cyclomatic: 14,
            },
        )]);

        let saved: Report = ratcheted_report(&baseline, &current);
        let saved = &saved["crates/a/src/lib.rs"];
        assert_eq!(saved.ploc, 80);
        assert_eq!(saved.lloc, 45);
        assert_eq!(saved.functions, 8);
        assert_eq!(saved.cyclomatic, 14);
    }
}
