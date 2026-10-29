//! Human and machine reports for the diff gate.
//!
//! Failures name the file, line, class, current score, base score, and limit. The fix is
//! tests or a refactor. The text never tells the reader to edit a baseline or add a waiver.

use serde::Serialize;

use super::model::{Class, Judgement, MetricJudgement};

pub const REPORT_FILE: &str = ".liberado/diff-gate-report.json";

const FIX_NEW: &str = "Fix: split the function or add tests. Do not add a baseline or a waiver.";
const FIX_LEGACY: &str =
    "Fix: add tests or refactor so the score does not rise. Do not add a baseline or a waiver.";

#[derive(Serialize)]
struct ReportFile {
    merge_base: String,
    base_rev: String,
    crap_epsilon: f64,
    cyclomatic_epsilon: f64,
    body_match_min: f64,
    limits: ReportLimits,
    base_crap: String,
    functions: Vec<ReportFn>,
    failures: Vec<String>,
}

#[derive(Serialize)]
struct ReportLimits {
    crap: f64,
    cyclomatic: f64,
}

#[derive(Serialize)]
struct ReportFn {
    file: String,
    function: String,
    line: u32,
    class: String,
    renamed_from: Option<ReportOrigin>,
    metrics: Vec<ReportMetric>,
}

#[derive(Serialize)]
struct ReportOrigin {
    file: String,
    function: String,
}

#[derive(Serialize)]
struct ReportMetric {
    metric: String,
    current: f64,
    base: Option<f64>,
    limit: f64,
    status: String,
}

pub fn failure_text(judgements: &[Judgement]) -> String {
    let mut lines = vec!["diff gate failed:".to_string()];
    for judgement in judgements.iter().filter(|item| item.failed()) {
        lines.push(format_heading(judgement));
        for metric in judgement.metrics.iter().filter(|metric| metric.failed) {
            lines.push(format!("  {}", format_metric(metric)));
        }
        lines.push(format!("  {}", fix_hint(&judgement.class)));
    }
    lines.join("\n")
}

pub fn render_report(
    merge_base: &str,
    base_rev: &str,
    limits_crap: f64,
    limits_cyclomatic: f64,
    base_crap: &str,
    judgements: &[Judgement],
) -> Result<String, Box<dyn std::error::Error>> {
    let report = ReportFile {
        merge_base: merge_base.to_string(),
        base_rev: base_rev.to_string(),
        crap_epsilon: super::CRAP_EPSILON,
        cyclomatic_epsilon: super::CYCLOMATIC_EPSILON,
        body_match_min: super::BODY_MATCH_MIN,
        limits: ReportLimits {
            crap: limits_crap,
            cyclomatic: limits_cyclomatic,
        },
        base_crap: base_crap.to_string(),
        functions: judgements.iter().map(report_fn).collect(),
        failures: judgements
            .iter()
            .filter(|item| item.failed())
            .map(format_heading)
            .collect(),
    };
    let mut json = serde_json::to_string_pretty(&report)?;
    json.push('\n');
    Ok(json)
}

fn report_fn(judgement: &Judgement) -> ReportFn {
    ReportFn {
        file: judgement.file.clone(),
        function: judgement.function.clone(),
        line: judgement.line,
        class: class_token(&judgement.class).to_string(),
        renamed_from: origin(&judgement.class),
        metrics: judgement.metrics.iter().map(report_metric).collect(),
    }
}

fn report_metric(metric: &MetricJudgement) -> ReportMetric {
    ReportMetric {
        metric: metric.kind.name().to_string(),
        current: metric.current,
        base: metric.base,
        limit: metric.limit,
        status: if metric.failed { "fail" } else { "pass" }.to_string(),
    }
}

fn format_heading(judgement: &Judgement) -> String {
    match &judgement.class {
        Class::RenamedFrom { file, function } => format!(
            "{}:{} {} (renamed from {file} {function})",
            judgement.file, judgement.line, judgement.function
        ),
        Class::New => format!(
            "{}:{} {} (new)",
            judgement.file, judgement.line, judgement.function
        ),
        Class::Legacy => format!(
            "{}:{} {} (touched legacy)",
            judgement.file, judgement.line, judgement.function
        ),
    }
}

fn format_metric(metric: &MetricJudgement) -> String {
    match metric.base {
        Some(base) => format!(
            "{} {:.3} > base {:.3} (limit {:.1})",
            metric.kind.name(),
            metric.current,
            base,
            metric.limit
        ),
        None => format!(
            "{} {:.3} > limit {:.1}",
            metric.kind.name(),
            metric.current,
            metric.limit
        ),
    }
}

fn fix_hint(class: &Class) -> &'static str {
    if class.is_legacy() {
        FIX_LEGACY
    } else {
        FIX_NEW
    }
}

fn class_token(class: &Class) -> &'static str {
    match class {
        Class::New => "new",
        Class::Legacy => "touched_legacy",
        Class::RenamedFrom { .. } => "renamed_from",
    }
}

fn origin(class: &Class) -> Option<ReportOrigin> {
    match class {
        Class::RenamedFrom { file, function } => Some(ReportOrigin {
            file: file.clone(),
            function: function.clone(),
        }),
        _ => None,
    }
}

pub fn emit_github_annotations(text: &str) {
    if std::env::var_os("GITHUB_ACTIONS").is_none() {
        return;
    }
    for line in text.lines().take(40) {
        if line.is_empty() || line == "diff gate failed:" {
            continue;
        }
        eprintln!("::error::{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::{failure_text, render_report};
    use crate::ci_cmd::diff_gate::model::{Class, Judgement, MetricJudgement, MetricKind};

    fn judgement() -> Judgement {
        Judgement {
            file: "crates/a.rs".into(),
            function: "work".into(),
            line: 12,
            class: Class::RenamedFrom {
                file: "crates/old.rs".into(),
                function: "old_work".into(),
            },
            metrics: vec![MetricJudgement {
                kind: MetricKind::Cyclomatic,
                current: 22.0,
                base: Some(21.0),
                limit: 20.0,
                failed: true,
            }],
        }
    }

    #[test]
    fn a_failure_names_the_class_the_scores_and_the_fix() {
        let text = failure_text(&[judgement()]);
        assert!(text.contains("crates/a.rs:12 work (renamed from crates/old.rs old_work)"));
        assert!(text.contains("cyclomatic 22.000 > base 21.000"));
        assert!(text.contains("does not rise"));
        assert!(!text.contains("waiver ceiling"));
        assert!(!text.contains("baseline.json"));
    }

    #[test]
    fn the_machine_report_records_the_epsilon_and_the_class() {
        let json = render_report(
            "abc",
            "origin/main",
            29.9,
            20.0,
            "not_needed",
            &[judgement()],
        )
        .expect("report json");
        assert!(json.contains("\"crap_epsilon\": 0.001"));
        assert!(json.contains("\"class\": \"renamed_from\""));
        assert!(json.contains("\"status\": \"fail\""));
        assert!(json.contains("old_work"));
    }
}
