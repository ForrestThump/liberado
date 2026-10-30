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
use std::process::{Command, ExitStatus, Stdio};

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
    require_llvm_cov(&log.root)?;
    measure_in_worktree(log, sha, &mut LiveCargo { log })
}

fn measure_in_worktree(
    log: &CiLog,
    sha: &str,
    run: &mut dyn CargoRunner,
) -> Result<String, Box<dyn std::error::Error>> {
    let outputs = prepare_base_coverage_outputs(&log.root)?;
    let cov_target = prepare_base_cov_target(&log.root)?;
    let tree = Worktree::add(&log.root, sha)?;
    measure_base_coverage(log, tree.path(), &outputs, &cov_target, run)
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
    note_seed(root, &dest);
    super::scrub_profiles(&dest)?;
    Ok(dest)
}

fn note_seed(root: &Path, dest: &Path) {
    let head = super::head_llvm_cov_target(root);
    let seeded = seed_head(&head, dest, root);
    eprintln!(
        "{}",
        seed_message(head.is_dir(), &seeded, &head.display().to_string())
    );
}

fn seed_head(head: &Path, dest: &Path, root: &Path) -> Result<u32, String> {
    if !head.is_dir() {
        return Err(String::new());
    }
    super::seed_registry_artifacts(head, dest, root).map_err(|error| error.to_string())
}

fn seed_message(present: bool, seeded: &Result<u32, String>, head: &str) -> String {
    if !present {
        return format!(
            "[diff gate] no HEAD llvm-cov target at {head}; base coverage target stays cold"
        );
    }
    match seeded {
        Ok(count) => {
            format!("[diff gate] seeded {count} registry artifact(s) from {head}")
        }
        Err(error) => format!("[diff gate] registry seed skipped: {error}"),
    }
}

fn measure_base_coverage(
    log: &CiLog,
    worktree: &Path,
    outputs: &BaseCoverageOutputs,
    cov_target: &Path,
    run: &mut dyn CargoRunner,
) -> Result<String, Box<dyn std::error::Error>> {
    run_lcov_job(log, worktree, outputs, cov_target, run)?;
    read_crap_job(log, worktree, outputs, run)
}

fn run_lcov_job(
    log: &CiLog,
    worktree: &Path,
    outputs: &BaseCoverageOutputs,
    cov_target: &Path,
    run: &mut dyn CargoRunner,
) -> Result<(), Box<dyn std::error::Error>> {
    let args = base_llvm_cov_args(&outputs.lcov)?;
    run_strings(log, worktree, &args, Some(cov_target), true, run)
}

fn read_crap_job(
    log: &CiLog,
    worktree: &Path,
    outputs: &BaseCoverageOutputs,
    run: &mut dyn CargoRunner,
) -> Result<String, Box<dyn std::error::Error>> {
    super::relativize_lcov(worktree, &outputs.lcov)?;
    let args = base_crap_report_args(&outputs.lcov, &outputs.json)?;
    run_strings(log, worktree, &args, None, false, run)?;
    read_relativized(worktree, &outputs.json)
}

fn run_strings(
    log: &CiLog,
    worktree: &Path,
    args: &[String],
    target: Option<&Path>,
    coverage: bool,
    run: &mut dyn CargoRunner,
) -> Result<(), Box<dyn std::error::Error>> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_job(
        log,
        &CargoJob {
            args: &refs,
            cwd: worktree,
            target,
            coverage,
        },
        run,
    )
}

fn read_relativized(worktree: &Path, json: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let path = json.to_str().ok_or("base CRAP json path is not utf-8")?;
    super::relativize_json_file(worktree, path)?;
    Ok(std::fs::read_to_string(json)?)
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
    score_prepared(log, cwd, output, keep_crap, target, &mut LiveCargo { log })
}

fn score_prepared(
    log: &CiLog,
    cwd: &Path,
    output: &str,
    keep_crap: bool,
    target: Option<&Path>,
    run: &mut dyn CargoRunner,
) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(cwd.join(".liberado"))?;
    let args = complexity_args(output);
    run_job(
        log,
        &CargoJob {
            args: &args,
            cwd,
            target,
            coverage: false,
        },
        run,
    )?;
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
    scores_from_json(root, &json, keep_crap)
}

fn scores_from_json(
    root: &Path,
    json: &str,
    keep_crap: bool,
) -> Result<Vec<ScoredFn>, Box<dyn std::error::Error>> {
    let mut scores = Vec::new();
    for entry in parse_entries(json)? {
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

struct CargoJob<'a> {
    args: &'a [&'a str],
    cwd: &'a Path,
    target: Option<&'a Path>,
    coverage: bool,
}

trait CargoRunner {
    fn run(&mut self, job: &CargoJob<'_>) -> Result<ExitStatus, Box<dyn std::error::Error>>;
}

struct LiveCargo<'a> {
    log: &'a CiLog,
}

impl CargoRunner for LiveCargo<'_> {
    fn run(&mut self, job: &CargoJob<'_>) -> Result<ExitStatus, Box<dyn std::error::Error>> {
        spawn_job(self.log, job)
    }
}

fn run_job(
    log: &CiLog,
    job: &CargoJob<'_>,
    run: &mut dyn CargoRunner,
) -> Result<(), Box<dyn std::error::Error>> {
    let shown = format!("cargo {}", job.args.join(" "));
    log.writeln(&format!("=== {shown} ==="))?;
    eprint!("[liberado ci] {shown} ... ");
    let _ = std::io::stderr().flush();
    let status = run.run(job)?;
    status_result(&shown, status)
}

fn status_result(shown: &str, status: ExitStatus) -> Result<(), Box<dyn std::error::Error>> {
    if status.success() {
        eprintln!("ok");
        return Ok(());
    }
    eprintln!("FAILED");
    Err(format!(
        "{shown} failed with {status}. Full log: {}",
        super::CI_LOG_FILE
    )
    .into())
}

fn spawn_job(log: &CiLog, job: &CargoJob<'_>) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    let mut command = prepared_command(log, job)?;
    Ok(command.status()?)
}

fn prepared_command(
    log: &CiLog,
    job: &CargoJob<'_>,
) -> Result<Command, Box<dyn std::error::Error>> {
    let mut command = cargo_command(log, job)?;
    apply_target(&mut command, job)?;
    finish_command(&mut command, job)?;
    Ok(command)
}

fn cargo_command(log: &CiLog, job: &CargoJob<'_>) -> Result<Command, Box<dyn std::error::Error>> {
    let stdout = std::fs::OpenOptions::new().append(true).open(&log.path)?;
    let stderr = std::fs::OpenOptions::new().append(true).open(&log.path)?;
    let mut command = std_command("cargo");
    command
        .args(job.args)
        .current_dir(job.cwd)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .env("CARGO_TERM_COLOR", "never");
    Ok(command)
}

fn apply_target(
    command: &mut Command,
    job: &CargoJob<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(dir) = job.target else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    command.env("CARGO_TARGET_DIR", dir);
    if job.coverage {
        command.env("CARGO_LLVM_COV_TARGET_DIR", dir);
    }
    Ok(())
}

fn finish_command(
    command: &mut Command,
    job: &CargoJob<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    super::coverage_tools::configure(command, job.cwd, job.args)?;
    super::job_budget::apply_compile_job_limit(command, std::ffi::OsStr::new("cargo"), job.args);
    Ok(())
}

#[cfg(test)]
#[path = "measure_tests.rs"]
mod tests;
