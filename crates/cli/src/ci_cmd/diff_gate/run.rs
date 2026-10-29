//! Run the diff gate and write `.liberado/diff-gate-report.json`.
//!
//! Cyclomatic mode does not measure coverage. CRAP mode measures HEAD coverage first. It
//! measures merge-base coverage only when a touched legacy function is over the CRAP limit,
//! and it reuses `.liberado/crap-base-<sha>.json` when that file is already present.

use std::path::Path;

use super::CiLog;
use super::cache::{cache_path, read_or_measure};
use super::classify::classify;
use super::decide::{MetricSet, base_crap_triggers, decide, failures};
use super::git_io::{ResolvedBase, load_diff, resolve_base};
use super::limits;
use super::measure::{self, fill_crap};
use super::model::{BaseCrapTrigger, Judgement, Limits};
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
        return finish(log, &resolved, limits, "not_needed", &[], &[]);
    }
    let head = measure::head_scores(log, metrics)?;
    let paths = diff.measured_base_paths();
    let mut base_scores = measure::base_cyclomatic(log, &resolved.merge_base, &paths)?;
    let classified = classify(&head, &base_scores, &diff);
    let passed = with_base_crap(
        log,
        &resolved,
        &head,
        &mut base_scores,
        &classified,
        limits,
        metrics,
    )?;
    finish(
        log,
        &resolved,
        limits,
        &passed.state,
        &passed.triggers,
        &passed.judgements,
    )
}

struct BaseCrapPass {
    judgements: Vec<Judgement>,
    state: String,
    triggers: Vec<BaseCrapTrigger>,
}

fn with_base_crap(
    log: &CiLog,
    resolved: &ResolvedBase,
    head: &[super::model::ScoredFn],
    base_scores: &mut [super::model::ScoredFn],
    classified: &[super::classify::Classified],
    limits: Limits,
    metrics: MetricSet,
) -> Result<BaseCrapPass, Box<dyn std::error::Error>> {
    let first = decide(head, base_scores, classified, limits, metrics);
    if !first.needs_base_crap {
        return Ok(BaseCrapPass {
            judgements: first.judgements,
            state: "not_needed".into(),
            triggers: Vec::new(),
        });
    }
    let triggers = base_crap_triggers(&first.judgements);
    let path = cache_path(&log.root, &resolved.merge_base);
    let loaded = read_or_measure(&path, true, || {
        log_triggers(log, &triggers);
        write_report(
            log,
            resolved,
            limits,
            "measuring",
            &triggers,
            &first.judgements,
        )?;
        measure::base_crap_json(log, &resolved.merge_base)
    })?;
    fill_crap(base_scores, loaded.json().unwrap_or(""))?;
    let second = decide(head, base_scores, classified, limits, metrics);
    Ok(BaseCrapPass {
        judgements: second.judgements,
        state: loaded.label().into(),
        triggers,
    })
}

fn log_triggers(log: &CiLog, triggers: &[BaseCrapTrigger]) {
    let header = "[diff gate] base CRAP measurement is required for touched legacy functions over the limit:";
    eprintln!("{header}");
    let _ = log.writeln(header);
    for trigger in triggers {
        let line = format!(
            "[diff gate] {}:{} {} CRAP {:.3}",
            trigger.file, trigger.line, trigger.function, trigger.crap
        );
        eprintln!("{line}");
        let _ = log.writeln(&line);
    }
}

fn finish(
    log: &CiLog,
    resolved: &ResolvedBase,
    limits: Limits,
    base_crap: &str,
    triggers: &[BaseCrapTrigger],
    judgements: &[Judgement],
) -> Result<(), Box<dyn std::error::Error>> {
    write_report(log, resolved, limits, base_crap, triggers, judgements)?;
    if failures(judgements).is_empty() {
        eprintln!("[diff gate] ok: {} changed function(s)", judgements.len());
        return Ok(());
    }
    let text = failure_text(judgements);
    report::emit_github_annotations(&text);
    eprintln!("{text}");
    Err(text.into())
}

fn write_report(
    log: &CiLog,
    resolved: &ResolvedBase,
    limits: Limits,
    base_crap: &str,
    triggers: &[BaseCrapTrigger],
    judgements: &[Judgement],
) -> Result<(), Box<dyn std::error::Error>> {
    let json = render_report(
        &resolved.merge_base,
        &resolved.base_rev,
        limits.crap,
        limits.cyclomatic,
        base_crap,
        triggers,
        judgements,
    )?;
    let path = log.root.join(report::REPORT_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    Ok(())
}
