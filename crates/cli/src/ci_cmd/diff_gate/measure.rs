//! Measure HEAD and the merge-base with cargo-crap.
//!
//! A complexity-only run strips the pessimistic CRAP field cargo-crap fills when it has no
//! coverage. Real CRAP comes only from a cargo-llvm-cov report. Base commands run in a
//! detached worktree. Cyclomatic builds use `.liberado/diff-gate-target`. Coverage builds
//! use `.liberado/diff-gate-cov-target`, seeded with registry artifacts from the HEAD
//! llvm-cov target. The base lcov file is an absolute path under the HEAD checkout, because
//! the worktree has no `.liberado` directory.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use liberado_common::process::std_command;
use serde::Deserialize;

use super::CiLog;
use super::decide::MetricSet;
use super::git_io::Worktree;
use super::model::{ScoredFn, slash};
use super::spans::function_end_line;

const COMPLEXITY_FILE: &str = ".liberado/function-complexity-current.json";
const BASE_COMPLEXITY_FILE: &str = ".liberado/diff-gate-complexity.json";
const BASE_LCOV_FILE: &str = ".liberado/diff-gate-base.lcov";
const BASE_CRAP_FILE: &str = ".liberado/diff-gate-base-crap.json";
const BASE_TARGET: &str = ".liberado/diff-gate-target";
const BASE_COV_TARGET: &str = ".liberado/diff-gate-cov-target";

struct BaseCoverageOutputs {
    lcov: PathBuf,
    json: PathBuf,
}

#[derive(Deserialize)]
struct CrapReport {
    entries: Vec<CrapEntry>,
}

#[derive(Deserialize)]
struct CrapEntry {
    file: String,
    function: String,
    line: u64,
    cyclomatic: f64,
    #[serde(default)]
    crap: Option<f64>,
}

struct ParsedEntry {
    file: String,
    function: String,
    line: u32,
    cyclomatic: f64,
    crap: Option<f64>,
}

pub fn head_scores(
    log: &CiLog,
    metrics: MetricSet,
) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>> {
    if metrics == MetricSet::Cyclomatic {
        return scores_at(log, &log.root, COMPLEXITY_FILE, false, None);
    }
    super::generate_lcov(log)?;
    super::write_crap_json(log, super::CURRENT_REPORT)?;
    read_scores(&log.root, super::CURRENT_REPORT, true)
}

pub fn base_cyclomatic(
    log: &CiLog,
    sha: &str,
    paths: &BTreeSet<String>,
) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let tree = Worktree::add(&log.root, sha)?;
    let target = log.root.join(BASE_TARGET);
    let scores = scores_at(
        log,
        tree.path(),
        BASE_COMPLEXITY_FILE,
        false,
        Some(target.as_path()),
    )?;
    Ok(filter_paths(scores, paths))
}

pub fn base_crap_json(log: &CiLog, sha: &str) -> Result<String, Box<dyn std::error::Error>> {
    let outputs = prepare_base_coverage_outputs(&log.root)?;
    let cov_target = prepare_base_cov_target(&log.root)?;
    let tree = Worktree::add(&log.root, sha)?;
    measure_base_coverage(log, tree.path(), &outputs, &cov_target)
}

fn prepare_base_coverage_outputs(
    root: &Path,
) -> Result<BaseCoverageOutputs, Box<dyn std::error::Error>> {
    let outputs = BaseCoverageOutputs {
        lcov: root.join(BASE_LCOV_FILE),
        json: root.join(BASE_CRAP_FILE),
    };
    if let Some(parent) = outputs.lcov.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(outputs)
}

fn prepare_base_cov_target(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dest = root.join(BASE_COV_TARGET);
    let head = super::head_llvm_cov_target(root);
    if head.is_dir() {
        match super::seed_registry_artifacts(&head, &dest, root) {
            Ok(count) => eprintln!(
                "[diff gate] seeded {count} registry artifact(s) from {}",
                head.display()
            ),
            Err(error) => eprintln!("[diff gate] registry seed skipped: {error}"),
        }
    } else {
        eprintln!(
            "[diff gate] no HEAD llvm-cov target at {}; base coverage target stays cold",
            head.display()
        );
    }
    super::scrub_profiles(&dest)?;
    Ok(dest)
}

fn measure_base_coverage(
    log: &CiLog,
    worktree: &Path,
    outputs: &BaseCoverageOutputs,
    cov_target: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    require_llvm_cov(&log.root)?;
    let lcov_args = base_llvm_cov_args(&outputs.lcov)?;
    let lcov_refs: Vec<&str> = lcov_args.iter().map(String::as_str).collect();
    run_coverage(log, worktree, &lcov_refs, cov_target)?;
    super::relativize_lcov(worktree, &outputs.lcov)?;
    let report_args = base_crap_report_args(&outputs.lcov, &outputs.json)?;
    let report_refs: Vec<&str> = report_args.iter().map(String::as_str).collect();
    run_cargo(log, worktree, &report_refs, None)?;
    let json_path = outputs
        .json
        .to_str()
        .ok_or("base CRAP json path is not utf-8")?;
    super::relativize_json_file(worktree, json_path)?;
    Ok(std::fs::read_to_string(&outputs.json)?)
}

fn base_llvm_cov_args(lcov: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    require_absolute_output(lcov)?;
    Ok(replace_flag(super::LLVM_COV_ARGS, "--output-path", lcov))
}

fn base_crap_report_args(
    lcov: &Path,
    json: &Path,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    require_absolute_output(lcov)?;
    require_absolute_output(json)?;
    let mut args = replace_flag(super::CRAP_REPORT_ARGS, "--lcov", lcov);
    args.push(json.to_string_lossy().into_owned());
    Ok(args)
}

fn replace_flag(args: &[&str], flag: &str, value: &Path) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        if args[index] == flag {
            out.push(flag.to_string());
            out.push(value.to_string_lossy().into_owned());
            index += 2;
            continue;
        }
        out.push(args[index].to_string());
        index += 1;
    }
    out
}

fn require_absolute_output(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_absolute() {
        return Err("base coverage output path must be absolute".into());
    }
    match path.parent() {
        Some(parent) if parent.is_dir() => Ok(()),
        Some(parent) => Err(format!(
            "base coverage output directory does not exist: {}",
            parent.display()
        )
        .into()),
        None => Err("base coverage output path has no parent".into()),
    }
}

pub fn fill_crap(scores: &mut [ScoredFn], json: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut by_key = BTreeMap::new();
    for entry in parse_entries(json)? {
        if let Some(crap) = entry.crap {
            by_key.insert((entry.file, entry.function, entry.line), crap);
        }
    }
    for score in scores.iter_mut() {
        let key = (slash(&score.file), score.function.clone(), score.line);
        if let Some(crap) = by_key.get(&key) {
            score.crap = Some(*crap);
        }
    }
    Ok(())
}

fn scores_at(
    log: &CiLog,
    cwd: &Path,
    output: &str,
    keep_crap: bool,
    target: Option<&Path>,
) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>> {
    super::require_crap(&log.root)?;
    std::fs::create_dir_all(cwd.join(".liberado"))?;
    run_cargo(log, cwd, &complexity_args(output), target)?;
    super::relativize_json_file(cwd, output)?;
    read_scores(cwd, output, keep_crap)
}

fn complexity_args(output: &str) -> Vec<&str> {
    vec![
        "crap",
        "--workspace",
        "--missing",
        "pessimistic",
        "--threshold",
        "1000000000",
        "--format",
        "json",
        "--sort",
        "file",
        "--output",
        output,
    ]
}

fn read_scores(
    root: &Path,
    relative: &str,
    keep_crap: bool,
) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>> {
    let json = std::fs::read_to_string(root.join(relative))?;
    let mut scores = Vec::new();
    for entry in parse_entries(&json)? {
        scores.push(score_one(root, entry, keep_crap)?);
    }
    Ok(scores)
}

fn parse_entries(json: &str) -> Result<Vec<ParsedEntry>, Box<dyn std::error::Error>> {
    let report: CrapReport = serde_json::from_str(json)?;
    Ok(report.entries.into_iter().map(parsed_entry).collect())
}

fn parsed_entry(entry: CrapEntry) -> ParsedEntry {
    ParsedEntry {
        file: slash(&entry.file),
        function: entry.function,
        line: u32::try_from(entry.line).unwrap_or(u32::MAX),
        cyclomatic: entry.cyclomatic,
        crap: entry.crap,
    }
}

fn score_one(
    root: &Path,
    entry: ParsedEntry,
    keep_crap: bool,
) -> Result<ScoredFn, Box<dyn std::error::Error>> {
    let source = std::fs::read_to_string(root.join(&entry.file)).unwrap_or_default();
    let end_line = if source.is_empty() {
        entry.line
    } else {
        function_end_line(&source, entry.line)
    };
    Ok(ScoredFn {
        file: entry.file,
        function: entry.function,
        line: entry.line,
        end_line,
        cyclomatic: entry.cyclomatic,
        crap: if keep_crap { entry.crap } else { None },
        body: body_slice(&source, entry.line, end_line),
    })
}

fn body_slice(source: &str, start: u32, end: u32) -> String {
    let skip = start.saturating_sub(1) as usize;
    let take = end.saturating_sub(start).saturating_add(1) as usize;
    source
        .lines()
        .skip(skip)
        .take(take)
        .collect::<Vec<_>>()
        .join("\n")
}

fn filter_paths(scores: Vec<ScoredFn>, paths: &BTreeSet<String>) -> Vec<ScoredFn> {
    scores
        .into_iter()
        .filter(|score| paths.contains(&score.file))
        .collect()
}

fn require_llvm_cov(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    super::require_tool(
        root,
        "llvm-cov",
        &["cargo", "llvm-cov", "--version"],
        "cargo install cargo-llvm-cov --locked",
    )
}

fn run_cargo(
    log: &CiLog,
    cwd: &Path,
    args: &[&str],
    target: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let shown = format!("cargo {}", args.join(" "));
    log.writeln(&format!("=== {shown} ==="))?;
    eprint!("[liberado ci] {shown} ... ");
    let _ = std::io::stderr().flush();
    let status = spawn_cargo(log, cwd, args, target, false)?;
    if status.success() {
        eprintln!("ok");
        Ok(())
    } else {
        eprintln!("FAILED");
        Err(format!(
            "{shown} failed with {status}. Full log: {}",
            super::CI_LOG_FILE
        )
        .into())
    }
}

fn run_coverage(
    log: &CiLog,
    cwd: &Path,
    args: &[&str],
    target: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let shown = format!("cargo {}", args.join(" "));
    log.writeln(&format!("=== {shown} ==="))?;
    eprint!("[liberado ci] {shown} ... ");
    let _ = std::io::stderr().flush();
    let status = spawn_cargo(log, cwd, args, Some(target), true)?;
    if status.success() {
        eprintln!("ok");
        Ok(())
    } else {
        eprintln!("FAILED");
        Err(format!(
            "{shown} failed with {status}. Full log: {}",
            super::CI_LOG_FILE
        )
        .into())
    }
}

fn spawn_cargo(
    log: &CiLog,
    cwd: &Path,
    args: &[&str],
    target: Option<&Path>,
    coverage_target: bool,
) -> Result<std::process::ExitStatus, Box<dyn std::error::Error>> {
    let stdout = std::fs::OpenOptions::new().append(true).open(&log.path)?;
    let stderr = std::fs::OpenOptions::new().append(true).open(&log.path)?;
    let mut command = std_command("cargo");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .env("CARGO_TERM_COLOR", "never");
    if let Some(dir) = target {
        std::fs::create_dir_all(dir)?;
        command.env("CARGO_TARGET_DIR", dir);
        if coverage_target {
            command.env("CARGO_LLVM_COV_TARGET_DIR", dir);
        }
    }
    super::coverage_tools::configure(&mut command, cwd, args)?;
    super::job_budget::apply_compile_job_limit(&mut command, std::ffi::OsStr::new("cargo"), args);
    Ok(command.status()?)
}

#[cfg(test)]
mod tests {
    use super::{
        base_crap_report_args, base_llvm_cov_args, fill_crap, parse_entries,
        prepare_base_coverage_outputs, score_one,
    };
    use std::path::Path;

    const JSON: &str = r#"{"entries":[{"file":"src\\work.rs","function":"edit","line":1,"cyclomatic":4.0,"crap":40.0}]}"#;

    #[test]
    fn a_complexity_score_drops_the_pessimistic_crap_field() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/work.rs"),
            "fn edit() {\n    let _ = 1;\n}\n",
        )
        .unwrap();
        let entry = parse_entries(JSON).unwrap().pop().unwrap();
        assert_eq!(entry.file, "src/work.rs");
        let score = score_one(dir.path(), entry, false).unwrap();
        assert!(score.crap.is_none());
        assert_eq!(score.line, 1);
        assert!(score.end_line >= 1);
        assert!(score.body.contains("fn edit"));
    }

    #[test]
    fn fill_crap_copies_coverage_onto_the_matching_function() {
        let mut scores = vec![
            score_one(
                std::path::Path::new("."),
                parse_entries(JSON).unwrap().pop().unwrap(),
                false,
            )
            .unwrap(),
        ];
        scores[0].crap = None;
        fill_crap(&mut scores, JSON).unwrap();
        assert_eq!(scores[0].crap, Some(40.0));
        let mut other = scores.clone();
        other[0].line = 9;
        other[0].crap = None;
        fill_crap(&mut other, JSON).unwrap();
        assert!(other[0].crap.is_none());
    }

    #[test]
    fn the_base_lcov_path_is_absolute_and_its_parent_is_created() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join(".liberado").exists());
        let outputs = prepare_base_coverage_outputs(dir.path()).unwrap();
        assert!(outputs.lcov.is_absolute());
        assert!(outputs.json.is_absolute());
        assert!(outputs.lcov.parent().unwrap().is_dir());
        let args = base_llvm_cov_args(&outputs.lcov).unwrap();
        let position = args.iter().position(|arg| arg == "--output-path").unwrap();
        assert_eq!(args[position + 1], outputs.lcov.to_string_lossy());
        assert!(Path::new(&args[position + 1]).is_absolute());
        assert!(!args.iter().any(|arg| arg == ".liberado/crap.lcov"));
        let report = base_crap_report_args(&outputs.lcov, &outputs.json).unwrap();
        let lcov_at = report.iter().position(|arg| arg == "--lcov").unwrap();
        assert_eq!(report[lcov_at + 1], outputs.lcov.to_string_lossy());
        assert_eq!(report.last().map(String::as_str), outputs.json.to_str());
    }

    #[test]
    fn a_relative_lcov_path_is_rejected_before_llvm_cov() {
        let error = base_llvm_cov_args(Path::new(".liberado/crap.lcov")).unwrap_err();
        assert!(error.to_string().contains("absolute"), "{error}");
    }
}
