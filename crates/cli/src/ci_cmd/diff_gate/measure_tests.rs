//! Tests for the diff-gate coverage and score commands.
//! Split from `measure.rs` so that file stays under the new-file function ceiling.

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

#[test]
fn seed_messages_name_a_cold_target_a_seed_and_a_skipped_seed() {
    let cold = super::seed_message(false, &Err(String::new()), "/head");
    assert!(cold.contains("stays cold"));
    assert!(cold.contains("/head"));
    let seeded = super::seed_message(true, &Ok(3), "/head");
    assert!(seeded.contains("seeded 3"));
    let skipped = super::seed_message(true, &Err("disk full".into()), "/head");
    assert!(skipped.contains("registry seed skipped"));
    assert!(skipped.contains("disk full"));
}

#[test]
fn score_prepared_reads_the_json_the_runner_writes() {
    let dir = tempfile::tempdir().unwrap();
    let log = crate::ci_cmd::CiLog::create(dir.path()).unwrap();
    let mut runner = WriteJson { fail: false };
    let scores = super::score_prepared(
        &log,
        dir.path(),
        ".liberado/diff-gate-complexity.json",
        false,
        None,
        &mut runner,
    )
    .unwrap();
    assert_eq!(scores.len(), 1);
    assert_eq!(scores[0].function, "edit");
    assert!(scores[0].crap.is_none());
    let log_text = std::fs::read_to_string(&log.path).unwrap();
    assert!(log_text.contains("cargo crap"));
}

#[test]
fn a_failed_score_command_returns_the_log_path() {
    let dir = tempfile::tempdir().unwrap();
    let log = crate::ci_cmd::CiLog::create(dir.path()).unwrap();
    let mut runner = WriteJson { fail: true };
    let error = super::score_prepared(
        &log,
        dir.path(),
        ".liberado/diff-gate-complexity.json",
        false,
        None,
        &mut runner,
    )
    .unwrap_err();
    assert!(error.to_string().contains("FAILED") || error.to_string().contains("failed"));
    assert!(error.to_string().contains(".liberado/ci.log"));
}

#[test]
fn read_scores_loads_a_file_under_the_root() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/work.rs"), "fn edit() {}\n").unwrap();
    std::fs::create_dir_all(dir.path().join(".liberado")).unwrap();
    std::fs::write(dir.path().join(".liberado/scores.json"), JSON).unwrap();
    let scores = super::read_scores(dir.path(), ".liberado/scores.json", true).unwrap();
    assert_eq!(scores[0].crap, Some(40.0));
    assert_eq!(scores[0].file, "src/work.rs");
}

#[test]
fn base_coverage_uses_absolute_outputs_and_removes_the_worktree() {
    let dir = init_repo();
    commit_file(dir.path(), "src/work.rs", "fn edit() {}\n", "base");
    let sha = git_out(dir.path(), &["rev-parse", "HEAD"]);
    let log = crate::ci_cmd::CiLog::create(dir.path()).unwrap();
    let mut runner = CoverageScript::default();
    let json = super::measure_in_worktree(&log, &sha, &mut runner).unwrap();
    assert!(json.contains("\"entries\""));
    assert!(!dir.path().join(".liberado/diff-gate-worktree").exists());
    assert!(runner.saw_absolute_lcov);
    assert!(runner.saw_coverage_target);
}

#[test]
fn a_failed_base_coverage_command_still_removes_the_worktree() {
    let dir = init_repo();
    commit_file(dir.path(), "src/work.rs", "fn edit() {}\n", "base");
    let sha = git_out(dir.path(), &["rev-parse", "HEAD"]);
    let log = crate::ci_cmd::CiLog::create(dir.path()).unwrap();
    let path = dir.path().join(".liberado/diff-gate-worktree");
    let mut runner = CoverageScript {
        fail: true,
        ..CoverageScript::default()
    };
    let error = super::measure_in_worktree(&log, &sha, &mut runner).unwrap_err();
    assert!(error.to_string().contains("failed"), "{error}");
    assert!(
        !path.exists(),
        "worktree remains after a failed measurement"
    );
}

struct WriteJson {
    fail: bool,
}

impl super::CargoRunner for WriteJson {
    fn run(
        &mut self,
        job: &super::CargoJob<'_>,
    ) -> Result<std::process::ExitStatus, Box<dyn std::error::Error>> {
        if self.fail {
            return Ok(exit_status(1));
        }
        let output = job
            .args
            .iter()
            .rposition(|arg| *arg == "--output")
            .map(|index| job.args[index + 1])
            .unwrap_or(".liberado/diff-gate-complexity.json");
        let path = job.cwd.join(output);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, JSON).unwrap();
        Ok(exit_status(0))
    }
}

#[derive(Default)]
struct CoverageScript {
    fail: bool,
    saw_absolute_lcov: bool,
    saw_coverage_target: bool,
}

impl super::CargoRunner for CoverageScript {
    fn run(
        &mut self,
        job: &super::CargoJob<'_>,
    ) -> Result<std::process::ExitStatus, Box<dyn std::error::Error>> {
        if job.coverage {
            self.saw_coverage_target = job.target.is_some_and(|path| path.is_absolute());
        }
        if self.fail {
            return Ok(exit_status(1));
        }
        if let Some(index) = job.args.iter().position(|arg| *arg == "--output-path") {
            let path = Path::new(job.args[index + 1]);
            self.saw_absolute_lcov = path.is_absolute();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, "SF:ignored\n").unwrap();
        }
        if let Some(path) = job.args.last() {
            let path = Path::new(path);
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                std::fs::write(path, "{\"entries\":[]}\n").unwrap();
            }
        }
        Ok(exit_status(0))
    }
}

fn exit_status(code: u8) -> std::process::ExitStatus {
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(u32::from(code))
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(i32::from(code) << 8)
    }
}

#[test]
fn exit_status_reports_the_same_code_on_every_platform() {
    let ok = exit_status(0);
    let failed = exit_status(1);
    assert!(ok.success());
    assert_eq!(ok.code(), Some(0));
    assert!(!failed.success());
    assert_eq!(failed.code(), Some(1));
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
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
    git(dir, &["add", "--", relative]);
    git(dir, &["commit", "-m", message]);
}
