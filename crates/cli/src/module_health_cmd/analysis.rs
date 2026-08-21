//! Adapter from rust-code-analysis JSON output to the stable health report.

use super::{
    ANALYSIS_DIR, FileMetrics, Report, TOOL, collect_json, exact, is_production_source, verify_tool,
};
use liberado_common::process::std_command;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct Analysis {
    metrics: AnalysisMetrics,
}

#[derive(Deserialize)]
struct AnalysisMetrics {
    loc: Loc,
    nom: Nom,
    cyclomatic: Aggregate,
}

#[derive(Deserialize)]
struct Loc {
    ploc: f64,
    lloc: f64,
}

#[derive(Deserialize)]
struct Nom {
    total: f64,
}

#[derive(Deserialize)]
struct Aggregate {
    sum: f64,
}

pub(super) fn analyze(root: &Path) -> Result<Report, Box<dyn std::error::Error>> {
    verify_tool()?;
    let output = prepare_analysis_dir(root)?;
    run_analysis(root)?;
    read_analysis_report(&output)
}

fn prepare_analysis_dir(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let output = root.join(ANALYSIS_DIR);
    if output.exists() {
        std::fs::remove_dir_all(&output)?;
    }
    std::fs::create_dir_all(&output)?;
    Ok(output)
}

fn run_analysis(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = std_command(TOOL)
        .current_dir(root)
        .args([
            "--metrics",
            "--paths",
            "crates",
            "--output-format",
            "json",
            "--output",
            ANALYSIS_DIR,
        ])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{TOOL} failed with {status}").into())
    }
}

fn read_analysis_report(output: &Path) -> Result<Report, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    collect_json(output, &mut files)?;
    let mut report = Report::new();
    for file in files {
        if let Some((source, metrics)) = read_source_metrics(output, &file)? {
            report.insert(source, metrics);
        }
    }
    Ok(report)
}

fn read_source_metrics(
    output: &Path,
    file: &Path,
) -> Result<Option<(String, FileMetrics)>, Box<dyn std::error::Error>> {
    let rel = file
        .strip_prefix(output)?
        .to_string_lossy()
        .replace('\\', "/");
    let source = rel.strip_suffix(".json").unwrap_or(&rel);
    if !is_production_source(source) {
        return Ok(None);
    }
    let analysis: Analysis = serde_json::from_slice(&std::fs::read(file)?)?;
    let metrics = FileMetrics {
        ploc: exact(analysis.metrics.loc.ploc, source, "ploc")?,
        lloc: exact(analysis.metrics.loc.lloc, source, "lloc")?,
        functions: exact(analysis.metrics.nom.total, source, "functions")?,
        cyclomatic: exact(analysis.metrics.cyclomatic.sum, source, "cyclomatic")?,
    };
    Ok(Some((source.to_owned(), metrics)))
}
