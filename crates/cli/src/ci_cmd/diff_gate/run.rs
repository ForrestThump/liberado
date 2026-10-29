//! Run the diff gate and write `.liberado/diff-gate-report.json`.
//!
//! Cyclomatic mode does not measure coverage. CRAP mode measures HEAD coverage first. It
//! measures merge-base coverage only when a touched legacy function is over the CRAP limit,
//! and it reuses `.liberado/crap-base-<sha>.json` when that file is already present.

use std::path::Path;

use super::CiLog;
use super::cache::{cache_path, read_or_measure};
use super::classify::classify;
use super::decide::{MetricSet, decide, failures};
use super::git_io::{ResolvedBase, load_diff, resolve_base};
use super::limits;
use super::measure::{self, fill_crap};
use super::model::{Judgement, Limits};
use super::report::{self, failure_text, render_report};

pub fn check_complexity(root: &Path, base: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let log = CiLog::create(root)?;
    execute(&log, base, MetricSet::Cyclomatic)
}

pub fn check_crap(log: &CiLog, base: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    execute(log, base, MetricSet::Both)
}

pub fn merge_base_sha(
    root: &Path,
    base: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(resolve_base(root, base)?.merge_base)
}

fn execute(
    log: &CiLog,
    base: Option<&str>,
    metrics: MetricSet,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = resolve_base(&log.root, base)?;
    let diff = load_diff(&log.root, &resolved.merge_base)?;
    let limits = limits::load(&log.root)?;
    if !diff.touches_rust() {
        return finish(log, &resolved, limits, "not_needed", &[]);
    }
    let head = measure::head_scores(log, metrics)?;
    let paths = diff.measured_base_paths();
    let mut base_scores = measure::base_cyclomatic(log, &resolved.merge_base, &paths)?;
    let classified = classify(&head, &base_scores, &diff);
    let (judgements, state) = with_base_crap(
        log,
        &resolved.merge_base,
        &head,
        &mut base_scores,
        &classified,
        limits,
        metrics,
    )?;
    finish(log, &resolved, limits, &state, &judgements)
}

fn with_base_crap(
    log: &CiLog,
    sha: &str,
    head: &[super::model::ScoredFn],
    base_scores: &mut [super::model::ScoredFn],
    classified: &[super::classify::Classified],
    limits: Limits,
    metrics: MetricSet,
) -> Result<(Vec<Judgement>, String), Box<dyn std::error::Error>> {
    let first = decide(head, base_scores, classified, limits, metrics);
    if !first.needs_base_crap {
        return Ok((first.judgements, "not_needed".into()));
    }
    let path = cache_path(&log.root, sha);
    let loaded = read_or_measure(&path, true, || measure::base_crap_json(log, sha))?;
    fill_crap(base_scores, loaded.json().unwrap_or(""))?;
    let second = decide(head, base_scores, classified, limits, metrics);
    Ok((second.judgements, loaded.label().into()))
}

fn finish(
    log: &CiLog,
    resolved: &ResolvedBase,
    limits: Limits,
    base_crap: &str,
    judgements: &[Judgement],
) -> Result<(), Box<dyn std::error::Error>> {
    let json = render_report(
        &resolved.merge_base,
        &resolved.base_rev,
        limits.crap,
        limits.cyclomatic,
        base_crap,
        judgements,
    )?;
    let path = log.root.join(report::REPORT_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    if failures(judgements).is_empty() {
        eprintln!("[diff gate] ok: {} changed function(s)", judgements.len());
        return Ok(());
    }
    let text = failure_text(judgements);
    report::emit_github_annotations(&text);
    eprintln!("{text}");
    Err(text.into())
}
