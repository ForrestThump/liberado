//! Human and machine reports for the diff gate.
//!
//! Failures name the file, line, class, current score, base score, and limit. The fix is
//! tests or a refactor. The text never tells the reader to edit a baseline or add a waiver.

use std::io::Write;

use serde::Serialize;

use super::model::{BaseCrapTrigger, Class, Judgement, MetricJudgement};

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
    base_crap_triggers: Vec<ReportTrigger>,
    functions: Vec<ReportFn>,
    failures: Vec<String>,
}

#[derive(Serialize)]
struct ReportTrigger {
    file: String,
    function: String,
    line: u32,
    crap: f64,
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
    triggers: &[BaseCrapTrigger],
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
        base_crap_triggers: triggers.iter().map(report_trigger).collect(),
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

fn report_trigger(trigger: &BaseCrapTrigger) -> ReportTrigger {
    ReportTrigger {
        file: trigger.file.clone(),
        function: trigger.function.clone(),
        line: trigger.line,
        crap: trigger.crap,
    }
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
    if actions_log_enabled() {
        emit_annotations(text, &mut std::io::stderr());
    }
}

fn actions_log_enabled() -> bool {
    std::env::var_os("GITHUB_ACTIONS").is_some()
}

pub fn emit_annotations(text: &str, out: &mut impl Write) {
    for line in text.lines().take(40).filter(|line| annotation_line(line)) {
        let _ = writeln!(out, "::error::{line}");
    }
}

fn annotation_line(line: &str) -> bool {
    !line.is_empty() && line != "diff gate failed:"
}

#[cfg(test)]
mod tests {
    use super::{failure_text, render_report};
    use crate::ci_cmd::diff_gate::model::{
        BaseCrapTrigger, Class, Judgement, MetricJudgement, MetricKind,
    };

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
            &[],
            &[judgement()],
        )
        .expect("report json");
        assert!(json.contains("\"crap_epsilon\": 0.001"));
        assert!(json.contains("\"class\": \"renamed_from\""));
        assert!(json.contains("\"status\": \"fail\""));
        assert!(json.contains("old_work"));
        assert!(json.contains("\"base_crap_triggers\": []"));
    }

    #[test]
    fn the_machine_report_names_the_base_crap_triggers() {
        let trigger = BaseCrapTrigger {
            file: "crates/a.rs".into(),
            line: 40,
            function: "legacy_work".into(),
            crap: 42.5,
        };
        let json = render_report(
            "abc",
            "origin/main",
            29.9,
            20.0,
            "measuring",
            &[trigger],
            &[],
        )
        .expect("report json");
        assert!(json.contains("\"base_crap\": \"measuring\""));
        assert!(json.contains("\"file\": \"crates/a.rs\""));
        assert!(json.contains("\"function\": \"legacy_work\""));
        assert!(json.contains("\"line\": 40"));
        assert!(json.contains("\"crap\": 42.5"));
    }

    #[test]
    fn annotations_skip_the_heading_and_stop_after_forty_lines() {
        let mut lines = vec!["diff gate failed:".to_string(), String::new()];
        for index in 1..=42 {
            lines.push(format!("crates/a.rs:{index} work (new)"));
        }
        let mut buf = Vec::new();
        super::emit_annotations(&lines.join("\n"), &mut buf);
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("::error::crates/a.rs:1 work (new)"));
        assert!(text.contains("::error::crates/a.rs:38 work (new)"));
        assert!(!text.contains("diff gate failed"));
        assert!(!text.contains("crates/a.rs:41"));
        assert!(!super::annotation_line(""));
        assert!(!super::annotation_line("diff gate failed:"));
        assert!(super::annotation_line("crates/a.rs:1 work (new)"));
    }
}
