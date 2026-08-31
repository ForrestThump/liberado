//! Repository checks that used to live in shell-specific preflight scripts.
//!
//! ## CRAP ratchet
//!
//! `crap-baseline.json` is the last best per-function score. GitHub only *reads* it
//! (`liberado ci crap` / `--fail-regression`, keyed by file + function name, not
//! line). A local `liberado ci` run that stays
//! green *writes* it (`liberado ci ratchet`). That is the same check-vs-write split
//! as `liberado docs crate-map` / `--write`.
//!
//! GitHub must not rewrite the file. Coverage is host-sensitive, and a bot commit
//! on `main` races every open PR.
//!
//! After a green *Linux* write, `just ci` stages `crap-baseline.json`. If that
//! is the only change, it amends HEAD (`--no-verify`, because this process just
//! ran the suite) so a subsequent `git push` includes the ratchet. If the tree
//! already has other dirty files, it only stages — the agent is about to commit
//! its own work.
//!
//! Coverage is host-sensitive. Non-Linux `just ci` / `liberado ci crap` checks
//! the 49.9 ceiling only (`--fail-above`). The per-function ratchet
//! (`--fail-regression`) runs on Linux, which is GitHub's Ubuntu job.
//! A current score below 10 is not a regression: cargo-crap `--min` drops it
//! before the detector runs, so a 4→5 move does not fail the job.
//!
//! `just ci` is `cargo run -p liberado-cli -- ci`. On Windows, `cargo test` cannot
//! overwrite a running `target/debug/liberado.exe` (Access is denied). A second
//! process still holds that path (`cargo run` waits on it). Before `cargo test`,
//! `liberado ci` *renames* the running image to `.liberado/liberado-ci` so cargo
//! can write a new artifact at the old path. Usage-only verbs do not move it.
//!
//! Child stdout/stderr go to `.liberado/ci.log`. The console prints the log path,
//! one `ok`/`FAILED` line per gate, and (on red) extracted `error[` / `FAILED` /
//! `panicked` / CRAP lines. The full log is always named so an agent can read it.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use liberado_common::process::std_command;
use serde_json::Value;

/// Pinned so a cargo-crap release cannot silently reshape the baseline schema.
const CARGO_CRAP_VERSION: &str = "0.4.3";
const BASELINE_FILE: &str = "crap-baseline.json";
const LCOV_FILE: &str = ".liberado/crap.lcov";
const CURRENT_REPORT: &str = ".liberado/crap-current.json";
const USAGE: &str = "usage: liberado ci [check|crap|crap-linux|ratchet|modules|modules-ratchet|complexity|complexity-ratchet|unwraps|unwraps-ratchet|ready|verify-ready]";
const VACATED_BIN: &str = "liberado-ci";
const CI_LOG_FILE: &str = ".liberado/ci.log";
const EXTRACT_MAX_LINES: usize = liberado_coder_core::FAILURE_EXTRACT_MAX_LINES;

/// New-function / `--fail-above` ceiling. Must match `.cargo-crap.toml` `threshold`.
const CRAP_CEILING: &str = "49.9";

/// cargo-crap `--min` keeps functions with `crap >= min`. Passing 10 drops a
/// current score below 10, so a 4→5 move is not a regression. Do not put this
/// in `.cargo-crap.toml`: that file is also read when writing the baseline, and
/// a filtered write would make a 4→50 jump look like a new function under the ceiling.
const CRAP_REGRESSION_MIN: &str = "10";

/// Report generation must not enforce `.cargo-crap.toml`'s `fail-above` policy. The explicit
/// compare that follows owns pass/fail and prints the offending functions. An effectively
/// unreachable threshold overrides the configured ceiling for this report-only command.
const CRAP_REPORT_THRESHOLD: &str = "1e308";
const CRAP_REPORT_ARGS: &[&str] = &[
    "crap",
    "--workspace",
    "--lcov",
    LCOV_FILE,
    "--format",
    "json",
    "--threshold",
    CRAP_REPORT_THRESHOLD,
    "--sort",
    "file",
    "--output",
];

/// llvm-cov flags live here, never after `--`. After `--` they become
/// test-binary arguments, and libtest rejects them (`Unrecognized option`).
/// `--ignore-run-fail` still writes the LCOV when a test is red (the test
/// job already owns pass/fail). That is how a host-local `config check`
/// failure cannot block the baseline write.
const LLVM_COV_ARGS: &[&str] = &[
    "llvm-cov",
    "--workspace",
    "--exclude",
    "liberado-webui",
    "--lcov",
    "--output-path",
    LCOV_FILE,
    "--ignore-run-fail",
];

/// Printed after `cargo crap` exits non-zero when a per-function score rose.
const CRAP_REGRESSION_HINT: &str = "\
CRAP check failed. A function's score went up vs crap-baseline.json \
(per-function ratchet: 40 cannot become 45, even under the 49.9 ceiling). \
A current score below 10 is ignored. cargo-crap named the functions above. \
Split the function or add tests until each score is at or below its baseline. \
Do not raise the baseline. `just ci` will not rewrite it while this check is red. \
Fix locally, then push.";

/// Printed after `cargo crap` exits non-zero when the baseline is still empty.
const CRAP_CEILING_HINT: &str = "\
CRAP check failed. A function is above the 49.9 ceiling (`--fail-above`). \
Split it or add tests. New functions must land below 50.";

/// One-line GitHub Actions annotation (newlines are not legal in `::error`).
const CRAP_REGRESSION_GH: &str = "\
A function CRAP score went up vs crap-baseline.json (per-function ratchet). \
Scores below 10 are ignored. Split the function or add tests. \
Do not raise the baseline. Linux `just ci` or this Ubuntu job is the check that matches the file.";

/// Banner when this host is not Linux: do not run `--fail-regression` here.
const CRAP_HOST_CEILING_ONLY: &str = "\
[liberado ci] this host is not Linux — ceiling only (49.9). \
GitHub's Ubuntu job runs the per-function ratchet.";

const CRAP_EMPTY_BASELINE: &str = "\
[liberado ci] crap-baseline.json has no entries yet — ceiling only (`--fail-above`). \
A green Linux `liberado ci ratchet` fills the per-function ratchet.";

const CRAP_COMPARE_SUMMARY: &str = "\
[liberado ci] CRAP compare against crap-baseline.json \
(per-function ratchet on Linux; scores below 10 are ignored; 49.9 is the new-function ceiling)";

const CRAP_CEILING_GH: &str = "\
A function is above the 49.9 CRAP ceiling. Split it or add tests. \
New functions must land below 50.";

mod coverage_tools;
/// Dispatch `liberado ci …`. No subcommand means the local full run (gates + ratchet).
mod dispatch;

pub use dispatch::run;

fn with_log(
    body: impl FnOnce(&CiLog) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    body(&CiLog::create(&repository_root()?)?)
}

/// One invocation's full child log. Truncated at the start of `liberado ci`.
pub(crate) struct CiLog {
    root: PathBuf,
    path: PathBuf,
}

impl CiLog {
    fn create(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        std::fs::create_dir_all(root.join(".liberado"))?;
        let path = root.join(CI_LOG_FILE);
        std::fs::write(
            &path,
            format!("# liberado ci — full log\n# {CI_LOG_FILE}\n"),
        )?;
        eprintln!("[liberado ci] full log: {CI_LOG_FILE}");
        Ok(Self {
            root: root.to_path_buf(),
            path,
        })
    }

    fn writeln(&self, line: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut file = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        writeln!(file, "{line}")?;
        Ok(())
    }
}

/// Move this process's image out of `target/{debug,release}`.
///
/// `cargo test --workspace` rebuilds `liberado.exe`. Windows refuses to overwrite
/// a running image (Access is denied). Re-exec of a copy does not help: the
/// original process stays alive until the child exits, so the path stays locked.
/// Rename vacates the cargo artifact path; this process keeps running from the
/// new name.
pub(crate) fn vacate_cargo_target_image() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not resolve current exe: {error}"),
        )
    })?;
    if !exe_lives_in_cargo_target(&exe) {
        return Ok(());
    }
    let dest = vacated_image_destination(&exe)?;
    move_running_image(&exe, &dest)
}

fn vacated_image_destination(exe: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dest_dir = repository_root()?.join(".liberado");
    std::fs::create_dir_all(&dest_dir)?;
    Ok(match exe.extension() {
        Some(ext) => dest_dir.join(VACATED_BIN).with_extension(ext),
        None => dest_dir.join(VACATED_BIN),
    })
}

fn move_running_image(exe: &Path, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&exe, &dest).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "could not move running image from {} to {}: {error}",
                exe.display(),
                dest.display()
            ),
        )
    })?;
    eprintln!(
        "[liberado ci] moved running image to {} so cargo can rebuild it",
        dest.display()
    );
    Ok(())
}

fn exe_lives_in_cargo_target(exe: &Path) -> bool {
    let hay = exe.to_string_lossy().replace('\\', "/");
    hay.contains("/debug/") || hay.contains("/release/") || hay.contains("/llvm-cov-target/")
}

/// Run the repository's local ship preflight (no CRAP llvm-cov).
///
/// The command list is deliberately kept here, rather than in a shell script, so the same
/// preflight works through the native `liberado` binary on every host OS.
fn check(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    vacate_cargo_target_image()?;
    run_cmd(log, "cargo", &["fmt", "--check"])?;
    crate::dependency_security_cmd::run(log)?;
    run_cmd(
        log,
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--exclude",
            "liberado-webui",
            "--all-targets",
            "--",
            "-D",
            "warnings",
            "-D",
            "clippy::cognitive_complexity",
        ],
    )?;
    run_cmd(log, "cargo", &["test", "--workspace", "--no-fail-fast"])?;
    crate::branch_cleaner_ci::run(log)
}

/// Full local CI: the ship preflight, then the CRAP check, then rewrite and stage the baseline.
/// Compare the current tree against `crap-baseline.json`. Never writes the baseline.
///
/// Always writes `.liberado/crap-current.json` (gitignored) so a red GitHub job
/// still has an Ubuntu-shaped report to commit as the next baseline. Coverage is
/// host-sensitive; a Windows-generated file fails `--fail-regression` on Ubuntu.
fn crap_check(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    generate_lcov(log)?;
    write_crap_json(log, CURRENT_REPORT)?;
    compare_to_baseline(log)
}

pub(crate) fn crap_for_root(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crap_check(&CiLog::create(root)?)
}

/// Check, then replace `crap-baseline.json` with this run's scores.
fn crap_ratchet(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    generate_lcov(log)?;
    compare_to_baseline(log)?;
    write_and_stage_ratcheted_baseline(log)
}

/// The baseline-write half of [`crap_ratchet`], split so each driver stays under the complexity
/// ceiling. Linux-only by policy: GitHub's Ubuntu job is the host of truth for per-function
/// scores, and coverage numbers are host-sensitive.
fn write_and_stage_ratcheted_baseline(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    if !cfg!(target_os = "linux") {
        eprintln!(
            "[liberado ci] {BASELINE_FILE} write is Linux-only \
             (GitHub's Ubuntu job is the host of truth). Compared only."
        );
        return Ok(());
    }
    write_baseline(log)?;
    announce_staged_baseline(stage_ratcheted_baseline(&log.root)?);
    Ok(())
}

fn announce_staged_baseline(outcome: StageOutcome) {
    match outcome {
        StageOutcome::Unchanged => {
            eprintln!("[liberado ci] {BASELINE_FILE} unchanged");
        }
        StageOutcome::Staged => {
            eprintln!(
                "[liberado ci] staged {BASELINE_FILE}; other dirty files present — not amending"
            );
        }
        StageOutcome::Amended => {
            eprintln!("[liberado ci] amended {BASELINE_FILE} onto HEAD");
        }
    }
}

fn generate_lcov(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    require_tool(
        &log.root,
        "llvm-cov",
        &["cargo", "llvm-cov", "--version"],
        "cargo install cargo-llvm-cov --locked",
    )?;
    std::fs::create_dir_all(log.root.join(".liberado"))?;
    run_cmd(log, "cargo", LLVM_COV_ARGS)?;
    relativize_lcov(&log.root)
}

fn compare_to_baseline(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    require_crap(&log.root)?;
    let fail_regression = announce_compare(log)?;
    run_cmd(log, "cargo", &compare_args(fail_regression))
        .map_err(|error| emit_crap_failure(fail_regression, error))
}

/// Record host/baseline banners in the log. Ceiling-only and empty-baseline
/// also go to the console: they change what this run compared.
fn announce_compare(log: &CiLog) -> Result<bool, Box<dyn std::error::Error>> {
    let has_entries = baseline_has_entries(&log.root.join(BASELINE_FILE));
    let fail_regression = uses_per_function_ratchet(has_entries);
    for line in compare_banners(has_entries, fail_regression) {
        log.writeln(line)?;
        if line == CRAP_HOST_CEILING_ONLY || line == CRAP_EMPTY_BASELINE {
            eprintln!("{line}");
        }
    }
    Ok(fail_regression)
}

fn compare_banners(has_entries: bool, fail_regression: bool) -> Vec<&'static str> {
    let mut lines = Vec::new();
    if !has_entries {
        lines.push(CRAP_EMPTY_BASELINE);
    } else if !fail_regression {
        lines.push(CRAP_HOST_CEILING_ONLY);
    }
    lines.push(CRAP_COMPARE_SUMMARY);
    lines
}

/// `--fail-regression` only on Linux. Coverage numbers are host-sensitive;
/// a Windows compare against the Ubuntu baseline false-fails (`explain_write` +127).
fn uses_per_function_ratchet(baseline_has_entries: bool) -> bool {
    baseline_has_entries && cfg!(target_os = "linux")
}

fn compare_args(fail_regression: bool) -> Vec<&'static str> {
    let mut args = vec![
        "crap",
        "--workspace",
        "--lcov",
        LCOV_FILE,
        "--fail-above",
        "--threshold",
        CRAP_CEILING,
    ];
    if fail_regression {
        args.extend_from_slice(&[
            "--min",
            CRAP_REGRESSION_MIN,
            "--baseline",
            BASELINE_FILE,
            "--fail-regression",
        ]);
    }
    args
}

fn write_baseline(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    write_crap_json(log, BASELINE_FILE)
}

fn write_crap_json(log: &CiLog, output: &str) -> Result<(), Box<dyn std::error::Error>> {
    require_crap(&log.root)?;
    let mut args = CRAP_REPORT_ARGS.to_vec();
    args.push(output);
    run_cmd(log, "cargo", &args)?;
    relativize_json_file(&log.root, output)
}

/// Strip the workspace root from an LCOV `SF:` path so Ubuntu CI and a Windows
/// `just ci` compare the same keys (`crates/foo.rs`, not `C:\Users\...\foo.rs`).
fn relativize_lcov(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let path = root.join(LCOV_FILE);
    let text = std::fs::read_to_string(&path)?;
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if let Some(file) = line.strip_prefix("SF:") {
            out.push_str("SF:");
            out.push_str(&repo_relative_source_path(root, file));
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    std::fs::write(path, out)?;
    Ok(())
}

pub(crate) fn relativize_json_file(
    root: &Path,
    relative: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = root.join(relative);
    let mut value: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    relativize_json_paths(root, &mut value);
    let serialized = serde_json::to_string_pretty(&value)?;
    std::fs::write(path, format!("{serialized}\n"))?;
    Ok(())
}

fn relativize_json_paths(root: &Path, value: &mut Value) {
    match value {
        Value::String(s) if looks_like_source_path(s) => {
            *s = repo_relative_source_path(root, s);
        }
        Value::Array(items) => {
            for item in items {
                relativize_json_paths(root, item);
            }
        }
        Value::Object(map) => {
            for nested in map.values_mut() {
                relativize_json_paths(root, nested);
            }
        }
        _ => {}
    }
}

fn looks_like_source_path(s: &str) -> bool {
    s.ends_with(".rs") && (s.contains('/') || s.contains('\\'))
}

fn repo_relative_source_path(root: &Path, file: &str) -> String {
    let file_norm = PathBuf::from(file.replace('/', std::path::MAIN_SEPARATOR_STR));
    if let Some(relative) = strip_root_prefix(root, &file_norm) {
        return relative;
    }
    if let Ok(canon_root) = root.canonicalize()
        && let Some(relative) = strip_root_prefix(&canon_root, &file_norm)
    {
        return relative;
    }
    file.replace('\\', "/")
}

fn strip_root_prefix(root: &Path, file: &Path) -> Option<String> {
    file.strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageOutcome {
    Unchanged,
    Staged,
    Amended,
}

/// Stage `crap-baseline.json`. Amend HEAD only when that file is the sole dirty path.
///
/// `--no-verify` on the amend: this process just ran the suite, and a pre-commit hook
/// that called `just ci` would recurse.
fn stage_ratcheted_baseline(root: &Path) -> Result<StageOutcome, Box<dyn std::error::Error>> {
    if !is_git_work_tree(root) {
        eprintln!("[liberado ci] not a git work tree — left {BASELINE_FILE} unstaged");
        return Ok(StageOutcome::Staged);
    }
    git(root, &["add", "--", BASELINE_FILE])?;
    let porcelain = git(root, &["status", "--porcelain"])?;
    let mut baseline_dirty = false;
    let mut other_dirty = false;
    for line in porcelain.lines() {
        let Some(path) = porcelain_path(line) else {
            continue;
        };
        if path == BASELINE_FILE {
            baseline_dirty = true;
        } else {
            other_dirty = true;
        }
    }
    if !baseline_dirty {
        return Ok(StageOutcome::Unchanged);
    }
    if other_dirty {
        return Ok(StageOutcome::Staged);
    }
    git(root, &["commit", "--amend", "--no-edit", "--no-verify"])?;
    Ok(StageOutcome::Amended)
}

fn is_git_work_tree(root: &Path) -> bool {
    std_command("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn porcelain_path(line: &str) -> Option<&str> {
    // `XY PATH` — two status columns, a space, then the path. Renames (`R  a -> b`)
    // are not expected for this file; treat the whole remainder as the path.
    if line.len() < 4 {
        return None;
    }
    Some(line[3..].trim())
}

fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = std_command("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| io::Error::new(error.kind(), format!("could not start git: {error}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into())
    }
}

fn require_crap(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    require_tool(
        root,
        "crap",
        &["cargo", "crap", "--version"],
        &format!("cargo install cargo-crap --version {CARGO_CRAP_VERSION} --locked"),
    )
}

fn require_tool(
    root: &Path,
    name: &str,
    probe: &[&str],
    install: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let status = std_command(probe[0])
        .args(&probe[1..])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(format!("cargo {name} is not installed. {install}").into()),
    }
}

/// Explain a red CRAP gate in the log, and as a GitHub annotation when CI set `GITHUB_ACTIONS`.
fn emit_crap_failure(
    has_ratchet: bool,
    error: Box<dyn std::error::Error>,
) -> Box<dyn std::error::Error> {
    crap_failure::emit_crap_failure_to(
        std::env::var_os("GITHUB_ACTIONS").is_some(),
        has_ratchet,
        error,
    )
}

mod crap_failure;

#[cfg(test)]
#[path = "ci_cmd/crap_failure_tests.rs"]
mod crap_failure_tests;

fn crap_failure_hint(has_ratchet: bool) -> &'static str {
    if has_ratchet {
        CRAP_REGRESSION_HINT
    } else {
        CRAP_CEILING_HINT
    }
}

fn baseline_has_entries(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    value
        .get("entries")
        .and_then(Value::as_array)
        .is_some_and(|entries| !entries.is_empty())
}

pub(crate) fn run_cmd(
    log: &CiLog,
    program: impl AsRef<OsStr>,
    args: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let program = program.as_ref();
    let gate = begin_cmd(log, program, args)?;
    finish_cmd(log, gate, coverage_tools::spawn_to_log(log, program, args))
}

struct GateStart {
    command: String,
    program: String,
    start: u64,
}

fn begin_cmd(
    log: &CiLog,
    program: &OsStr,
    args: &[&str],
) -> Result<GateStart, Box<dyn std::error::Error>> {
    let shown = program.to_string_lossy().into_owned();
    let command = format!("{shown} {}", args.join(" "));
    log.writeln(&format!("=== {command} ==="))?;
    eprint!("[liberado ci] {command} ... ");
    let _ = io::stderr().flush();
    Ok(GateStart {
        command,
        program: shown,
        start: std::fs::metadata(&log.path)?.len(),
    })
}

fn finish_cmd(
    log: &CiLog,
    gate: GateStart,
    spawned: io::Result<std::process::ExitStatus>,
) -> Result<(), Box<dyn std::error::Error>> {
    match spawned {
        Ok(status) if status.success() => {
            eprintln!("ok");
            Ok(())
        }
        Ok(status) => gate_failed(log, &gate, format!("{} failed with {status}", gate.command)),
        Err(error) => gate_failed(
            log,
            &gate,
            format!("could not start {}: {error}", gate.program),
        ),
    }
}

fn gate_failed(
    log: &CiLog,
    gate: &GateStart,
    reason: String,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("FAILED");
    log.writeln(&reason)?;
    let extracted = extract_ci_failures(&read_log_since(&log.path, gate.start)?);
    if !extracted.is_empty() {
        eprintln!("\n{extracted}\n");
    }
    eprintln!("----------\nFull log: {CI_LOG_FILE}\n----------");
    if extracted.is_empty() {
        return Err(format!("{reason}\nFull log: {CI_LOG_FILE}").into());
    }
    Err(format!("{reason}\nFull log: {CI_LOG_FILE}\n\n{extracted}").into())
}

fn read_log_since(path: &Path, start: u64) -> Result<String, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let skip = (start as usize).min(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[skip..]).into_owned())
}

/// Pull compiler, test, and CRAP failures out of a child log so the agent
/// does not have to scan compile progress or passing crates.
fn extract_ci_failures(output: &str) -> String {
    liberado_coder_core::extract_failures_capped(output, EXTRACT_MAX_LINES, Some(CI_LOG_FILE))
}

fn repository_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut candidate = std::env::current_dir()?;
    loop {
        if candidate.join("Cargo.toml").is_file() && candidate.join("crates").is_dir() {
            return Ok(candidate);
        }
        if !candidate.pop() {
            break;
        }
    }

    Err("liberado ci must run inside a Liberado repository".into())
}

#[cfg(test)]
#[path = "ci_cmd_usage_tests.rs"]
mod ci_cmd_usage_tests;

#[cfg(test)]
#[path = "ci_cmd_tests.rs"]
mod ci_cmd_tests;
