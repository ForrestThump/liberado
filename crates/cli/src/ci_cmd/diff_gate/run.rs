//! Run the diff gate and write `.liberado/diff-gate-report.json`.
//!
//! Cyclomatic mode does not measure coverage. CRAP mode measures HEAD coverage first. It
//! measures merge-base coverage only when a touched legacy function is over the CRAP limit,
//! and it reuses `.liberado/crap-base-<sha>.json` when that file is already present.

use std::io::Write;
use std::path::Path;

use super::CiLog;
use super::cache::{cache_path, read_or_measure};
use super::classify::classify;
use super::decide::{MetricSet, base_crap_triggers, decide, failures};
use super::git_io::{ResolvedBase, load_diff, resolve_base};
use super::hunks::ParsedDiff;
use super::limits;
use super::measure::{self, fill_crap};
use super::model::{BaseCrapTrigger, Judgement, Limits, ScoredFn};
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

trait Measure {
    fn head(
        &mut self,
        log: &CiLog,
        metrics: MetricSet,
    ) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>>;
    fn base_cyclomatic(
        &mut self,
        log: &CiLog,
        sha: &str,
        paths: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>>;
    fn base_crap(&mut self, log: &CiLog, sha: &str) -> Result<String, Box<dyn std::error::Error>>;
}

struct LiveMeasure;

impl Measure for LiveMeasure {
    fn head(
        &mut self,
        log: &CiLog,
        metrics: MetricSet,
    ) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>> {
        measure::head_scores(log, metrics)
    }

    fn base_cyclomatic(
        &mut self,
        log: &CiLog,
        sha: &str,
        paths: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>> {
        measure::base_cyclomatic(log, sha, paths)
    }

    fn base_crap(&mut self, log: &CiLog, sha: &str) -> Result<String, Box<dyn std::error::Error>> {
        measure::base_crap_json(log, sha)
    }
}

fn execute(
    log: &CiLog,
    base: Option<&str>,
    metrics: MetricSet,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = resolve_base(&log.root, base)?;
    let diff = load_diff(&log.root, &resolved.merge_base)?;
    let limits = limits::load(&log.root)?;
    gate(log, &resolved, &diff, limits, metrics, &mut LiveMeasure)
}

fn gate(
    log: &CiLog,
    resolved: &ResolvedBase,
    diff: &ParsedDiff,
    limits: Limits,
    metrics: MetricSet,
    measure: &mut dyn Measure,
) -> Result<(), Box<dyn std::error::Error>> {
    if !diff.touches_rust() {
        return finish(log, resolved, limits, "not_needed", &[], &[]);
    }
    judge(log, resolved, diff, limits, metrics, measure)
}

fn judge(
    log: &CiLog,
    resolved: &ResolvedBase,
    diff: &ParsedDiff,
    limits: Limits,
    metrics: MetricSet,
    measure: &mut dyn Measure,
) -> Result<(), Box<dyn std::error::Error>> {
    let head = measure.head(log, metrics)?;
    let paths = diff.measured_base_paths();
    let mut base_scores = measure.base_cyclomatic(log, &resolved.merge_base, &paths)?;
    let classified = classify(&head, &base_scores, diff);
    let mut scores = Scores {
        head: &head,
        base_scores: &mut base_scores,
        classified: &classified,
        limits,
        metrics,
    };
    let passed = with_base_crap(log, resolved, &mut scores, measure)?;
    finish(
        log,
        resolved,
        scores.limits,
        &passed.state,
        &passed.triggers,
        &passed.judgements,
    )
}

struct Scores<'a> {
    head: &'a [ScoredFn],
    base_scores: &'a mut [ScoredFn],
    classified: &'a [super::classify::Classified],
    limits: Limits,
    metrics: MetricSet,
}

struct BaseCrapPass {
    judgements: Vec<Judgement>,
    state: String,
    triggers: Vec<BaseCrapTrigger>,
}

fn with_base_crap(
    log: &CiLog,
    resolved: &ResolvedBase,
    scores: &mut Scores<'_>,
    measure: &mut dyn Measure,
) -> Result<BaseCrapPass, Box<dyn std::error::Error>> {
    let first = decide(
        scores.head,
        scores.base_scores,
        scores.classified,
        scores.limits,
        scores.metrics,
    );
    if !first.needs_base_crap {
        return Ok(BaseCrapPass {
            judgements: first.judgements,
            state: "not_needed".into(),
            triggers: Vec::new(),
        });
    }
    load_base_crap(log, resolved, scores, &first.judgements, measure)
}

fn load_base_crap(
    log: &CiLog,
    resolved: &ResolvedBase,
    scores: &mut Scores<'_>,
    first: &[Judgement],
    measure: &mut dyn Measure,
) -> Result<BaseCrapPass, Box<dyn std::error::Error>> {
    let triggers = base_crap_triggers(first);
    let path = cache_path(&log.root, &resolved.merge_base);
    let loaded = read_or_measure(&path, true, || {
        log_triggers(log, &triggers);
        write_report(log, resolved, scores.limits, "measuring", &triggers, first)?;
        measure.base_crap(log, &resolved.merge_base)
    })?;
    fill_crap(scores.base_scores, loaded.json().unwrap_or(""))?;
    let second = decide(
        scores.head,
        scores.base_scores,
        scores.classified,
        scores.limits,
        scores.metrics,
    );
    Ok(BaseCrapPass {
        judgements: second.judgements,
        state: loaded.label().into(),
        triggers,
    })
}

fn log_triggers(log: &CiLog, triggers: &[BaseCrapTrigger]) {
    let mut err = std::io::stderr();
    let _ = write_triggers(log, triggers, &mut err);
}

fn write_triggers(
    log: &CiLog,
    triggers: &[BaseCrapTrigger],
    err: &mut impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let header = "[diff gate] base CRAP measurement is required for touched legacy functions over the limit:";
    write_line(log, err, header)?;
    for trigger in triggers {
        write_line(log, err, &format_trigger(trigger))?;
    }
    Ok(())
}

fn write_line(
    log: &CiLog,
    err: &mut impl Write,
    line: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    writeln!(err, "{line}")?;
    log.writeln(line)?;
    Ok(())
}

fn format_trigger(trigger: &BaseCrapTrigger) -> String {
    format!(
        "[diff gate] {}:{} {} CRAP {:.3}",
        trigger.file, trigger.line, trigger.function, trigger.crap
    )
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
    write_under(&log.root, report::REPORT_FILE, &json)
}

fn write_under(root: &Path, relative: &str, json: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::super::decide::MetricSet;
    use super::super::git_io::ResolvedBase;
    use super::super::hunks::{FileChange, ParsedDiff};
    use super::super::model::{Limits, ScoredFn};
    use super::{check_complexity, execute, gate, judge, merge_base_sha, write_triggers};

    const BASE_JSON: &str = r#"{"entries":[{"file":"src/work.rs","function":"edit","line":1,"cyclomatic":1.0,"crap":40.0}]}"#;

    fn limits() -> Limits {
        Limits {
            crap: 29.9,
            cyclomatic: 20.0,
        }
    }

    fn resolved() -> ResolvedBase {
        ResolvedBase {
            base_rev: "origin/main".into(),
            merge_base: "abc".into(),
        }
    }

    fn score(cyc: f64, crap: Option<f64>) -> ScoredFn {
        ScoredFn {
            file: "src/work.rs".into(),
            function: "edit".into(),
            line: 1,
            end_line: 3,
            cyclomatic: cyc,
            crap,
            body: "fn edit() {}".into(),
        }
    }

    fn rust_diff() -> ParsedDiff {
        ParsedDiff {
            files: vec![FileChange {
                old_path: Some("src/work.rs".into()),
                new_path: Some("src/work.rs".into()),
                touched_lines: BTreeSet::from([2]),
                deleted: false,
                is_rename: false,
            }],
        }
    }

    struct Fake {
        head: Vec<ScoredFn>,
        base: Vec<ScoredFn>,
        json: Result<String, String>,
        measured: bool,
    }

    impl super::Measure for Fake {
        fn head(
            &mut self,
            _log: &super::super::CiLog,
            _metrics: MetricSet,
        ) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>> {
            Ok(self.head.clone())
        }

        fn base_cyclomatic(
            &mut self,
            _log: &super::super::CiLog,
            _sha: &str,
            _paths: &BTreeSet<String>,
        ) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>> {
            Ok(self.base.clone())
        }

        fn base_crap(
            &mut self,
            _log: &super::super::CiLog,
            _sha: &str,
        ) -> Result<String, Box<dyn std::error::Error>> {
            self.measured = true;
            self.json.clone().map_err(|error| error.into())
        }
    }

    fn log(dir: &Path) -> super::super::CiLog {
        super::super::CiLog::create(dir).unwrap()
    }

    #[test]
    fn a_diff_without_rust_writes_not_needed() {
        let dir = tempfile::tempdir().unwrap();
        let written = gate(
            &log(dir.path()),
            &resolved(),
            &ParsedDiff::default(),
            limits(),
            MetricSet::Cyclomatic,
            &mut Fake {
                head: Vec::new(),
                base: Vec::new(),
                json: Ok(BASE_JSON.into()),
                measured: false,
            },
        );
        assert!(written.is_ok());
        let report =
            std::fs::read_to_string(dir.path().join(".liberado/diff-gate-report.json")).unwrap();
        assert!(report.contains("\"base_crap\": \"not_needed\""));
    }

    #[test]
    fn a_new_function_under_the_limit_passes_and_over_the_limit_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake {
            head: vec![score(2.0, None)],
            base: Vec::new(),
            json: Ok(BASE_JSON.into()),
            measured: false,
        };
        judge(
            &log(dir.path()),
            &resolved(),
            &rust_diff(),
            limits(),
            MetricSet::Cyclomatic,
            &mut fake,
        )
        .unwrap();
        assert!(!fake.measured);
        let mut over = Fake {
            head: vec![score(25.0, None)],
            base: Vec::new(),
            json: Ok(BASE_JSON.into()),
            measured: false,
        };
        let error = judge(
            &log(dir.path()),
            &resolved(),
            &rust_diff(),
            limits(),
            MetricSet::Cyclomatic,
            &mut over,
        )
        .unwrap_err();
        assert!(error.to_string().contains("diff gate failed"));
        assert!(error.to_string().contains("(new)"));
    }

    #[test]
    fn an_over_limit_legacy_function_names_itself_before_base_crap() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake {
            head: vec![score(1.0, Some(40.0))],
            base: vec![score(1.0, None)],
            json: Ok(BASE_JSON.into()),
            measured: false,
        };
        let ci = log(dir.path());
        judge(
            &ci,
            &resolved(),
            &rust_diff(),
            limits(),
            MetricSet::Both,
            &mut fake,
        )
        .unwrap();
        assert!(fake.measured);
        let report =
            std::fs::read_to_string(dir.path().join(".liberado/diff-gate-report.json")).unwrap();
        assert!(report.contains("\"base_crap\": \"measured\""));
        assert!(report.contains("\"function\": \"edit\""));
        let logged = std::fs::read_to_string(&ci.path).unwrap();
        assert!(logged.contains("src/work.rs:1 edit CRAP 40.000"));
        let mut buf = Vec::new();
        write_triggers(
            &ci,
            &[super::super::model::BaseCrapTrigger {
                file: "src/work.rs".into(),
                line: 1,
                function: "edit".into(),
                crap: 40.0,
            }],
            &mut buf,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("src/work.rs:1 edit CRAP 40.000"));
    }

    #[test]
    fn a_failed_base_measurement_keeps_the_trigger_report() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake {
            head: vec![score(1.0, Some(40.0))],
            base: vec![score(1.0, None)],
            json: Err("llvm-cov failed".into()),
            measured: false,
        };
        let error = judge(
            &log(dir.path()),
            &resolved(),
            &rust_diff(),
            limits(),
            MetricSet::Both,
            &mut fake,
        )
        .unwrap_err();
        assert!(error.to_string().contains("llvm-cov failed"));
        let report =
            std::fs::read_to_string(dir.path().join(".liberado/diff-gate-report.json")).unwrap();
        assert!(report.contains("\"base_crap\": \"measuring\""));
        assert!(report.contains("\"crap\": 40.0"));
    }

    #[test]
    fn a_cached_base_report_skips_measurement() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".liberado")).unwrap();
        std::fs::write(dir.path().join(".liberado/crap-base-abc.json"), BASE_JSON).unwrap();
        let mut fake = Fake {
            head: vec![score(1.0, Some(40.0))],
            base: vec![score(1.0, None)],
            json: Err("should not measure".into()),
            measured: false,
        };
        judge(
            &log(dir.path()),
            &resolved(),
            &rust_diff(),
            limits(),
            MetricSet::Crap,
            &mut fake,
        )
        .unwrap();
        assert!(!fake.measured);
        let report =
            std::fs::read_to_string(dir.path().join(".liberado/diff-gate-report.json")).unwrap();
        assert!(report.contains("\"base_crap\": \"cache_hit\""));
    }

    #[test]
    fn execute_on_a_repo_without_rust_writes_not_needed() {
        let dir = init_repo();
        commit_file(dir.path(), "README.md", "hello\n", "base");
        write_limits(dir.path());
        check_complexity(dir.path(), Some("HEAD")).unwrap();
        let sha = git_out(dir.path(), &["rev-parse", "HEAD"]);
        assert_eq!(merge_base_sha(dir.path(), Some("HEAD")).unwrap(), sha);
        let report =
            std::fs::read_to_string(dir.path().join(".liberado/diff-gate-report.json")).unwrap();
        assert!(report.contains("\"base_crap\": \"not_needed\""));
    }

    #[test]
    fn execute_fails_when_the_limit_files_are_missing() {
        let dir = init_repo();
        commit_file(dir.path(), "README.md", "hello\n", "base");
        let error = execute(&log(dir.path()), Some("HEAD"), MetricSet::Cyclomatic).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("cargo-crap")
                || text.contains("function-complexity")
                || text.contains(".toml")
                || text.contains("os error"),
            "{text}"
        );
    }

    fn write_limits(root: &Path) {
        std::fs::write(root.join(".cargo-crap.toml"), "threshold = 29.9\n").unwrap();
        std::fs::create_dir_all(root.join("code-metrics")).unwrap();
        std::fs::write(
            root.join("code-metrics/function-complexity.toml"),
            "new_function_ceiling = 20\n",
        )
        .unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        git(dir.path(), &["config", "core.autocrlf", "false"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        dir
    }

    fn commit_file(dir: &Path, relative: &str, body: &str, message: &str) {
        std::fs::write(dir.join(relative), body).unwrap();
        git(dir, &["add", "--", relative]);
        git(dir, &["commit", "-m", message]);
    }
}
