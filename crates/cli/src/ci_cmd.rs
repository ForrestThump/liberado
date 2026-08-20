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
//! the 150 ceiling only (`--fail-above`). The per-function ratchet
//! (`--fail-regression`) runs on Linux, which is GitHub's Ubuntu job.
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
const USAGE: &str = "usage: liberado ci [check|crap|ratchet]";
const VACATED_BIN: &str = "liberado-ci";
const CI_LOG_FILE: &str = ".liberado/ci.log";
const EXTRACT_MAX_LINES: usize = 80;

/// New-function / `--fail-above` ceiling. Must match `.cargo-crap.toml` `threshold`.
const CRAP_CEILING: &str = "150";

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
(per-function ratchet: 50 cannot become 60, even under the 150 ceiling). \
cargo-crap named the functions above. Split the function or add tests until \
each score is at or below its baseline. Do not raise the baseline. \
`just ci` will not rewrite it while this check is red. Fix locally, then push.";

/// Printed after `cargo crap` exits non-zero when the baseline is still empty.
const CRAP_CEILING_HINT: &str = "\
CRAP check failed. A function is above the 150 ceiling (`--fail-above`). \
Split it or add tests. New functions must land at or below 150.";

/// One-line GitHub Actions annotation (newlines are not legal in `::error`).
const CRAP_REGRESSION_GH: &str = "\
A function CRAP score went up vs crap-baseline.json (per-function ratchet). \
Split the function or add tests. Do not raise the baseline. \
Linux `just ci` or this Ubuntu job is the check that matches the file.";

/// Banner when this host is not Linux: do not run `--fail-regression` here.
const CRAP_HOST_CEILING_ONLY: &str = "\
[liberado ci] this host is not Linux — ceiling only (150). \
GitHub's Ubuntu job runs the per-function ratchet.";

const CRAP_EMPTY_BASELINE: &str = "\
[liberado ci] crap-baseline.json has no entries yet — ceiling only (`--fail-above`). \
A green Linux `liberado ci ratchet` fills the per-function ratchet.";

const CRAP_COMPARE_SUMMARY: &str = "\
[liberado ci] CRAP compare against crap-baseline.json \
(per-function ratchet on Linux; 150 is the new-function ceiling)";

const CRAP_CEILING_GH: &str = "\
A function is above the 150 CRAP ceiling. Split it or add tests. \
New functions must land at or below 150.";

/// Dispatch `liberado ci …`. No subcommand means the local full run (gates + ratchet).
pub fn run(args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args.peekable();
    match args.next().as_deref() {
        None => with_log(local_run),
        Some("check") if args.peek().is_none() => with_log(check),
        Some("crap") if args.peek().is_none() => with_log(crap_check),
        Some("ratchet") if args.peek().is_none() => with_log(crap_ratchet),
        _ => Err(USAGE.into()),
    }
}

fn with_log(
    body: impl FnOnce(&CiLog) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    body(&CiLog::create(&repository_root()?)?)
}

/// One invocation's full child log. Truncated at the start of `liberado ci`.
struct CiLog {
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
fn vacate_cargo_target_image() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not resolve current exe: {error}"),
        )
    })?;
    if !exe_lives_in_cargo_target(&exe) {
        return Ok(());
    }
    let dest_dir = repository_root()?.join(".liberado");
    std::fs::create_dir_all(&dest_dir)?;
    let dest = match exe.extension() {
        Some(ext) => dest_dir.join(VACATED_BIN).with_extension(ext),
        None => dest_dir.join(VACATED_BIN),
    };
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
    hay.contains("/target/debug/")
        || hay.contains("/target/release/")
        || hay.contains("/target/llvm-cov-target/")
}

/// Run the repository's local ship preflight (no CRAP llvm-cov).
///
/// The command list is deliberately kept here, rather than in a shell script, so the same
/// preflight works through the native `liberado` binary on every host OS.
fn check(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    vacate_cargo_target_image()?;
    run_cmd(log, "cargo", &["fmt", "--check"])?;
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
    run_cmd(log, "cargo", &["deny", "check"])?;
    Ok(())
}

/// Full local CI: the ship preflight, then the CRAP check, then rewrite and stage the baseline.
fn local_run(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    check(log)?;
    crap_ratchet(log)
}

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

/// Check, then replace `crap-baseline.json` with this run's scores.
fn crap_ratchet(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    generate_lcov(log)?;
    compare_to_baseline(log)?;
    if !cfg!(target_os = "linux") {
        eprintln!(
            "[liberado ci] {BASELINE_FILE} write is Linux-only \
             (GitHub's Ubuntu job is the host of truth). Compared only."
        );
        return Ok(());
    }
    write_baseline(log)?;
    match stage_ratcheted_baseline(&log.root)? {
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
    Ok(())
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
        args.extend_from_slice(&["--baseline", BASELINE_FILE, "--fail-regression"]);
    }
    args
}

fn write_baseline(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    write_crap_json(log, BASELINE_FILE)
}

fn write_crap_json(log: &CiLog, output: &str) -> Result<(), Box<dyn std::error::Error>> {
    require_crap(&log.root)?;
    run_cmd(
        log,
        "cargo",
        &[
            "crap",
            "--workspace",
            "--lcov",
            LCOV_FILE,
            "--format",
            "json",
            "--sort",
            "file",
            "--output",
            output,
        ],
    )?;
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

fn relativize_json_file(root: &Path, relative: &str) -> Result<(), Box<dyn std::error::Error>> {
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
    let hint = crap_failure_hint(has_ratchet);
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        let title = if has_ratchet {
            "CRAP regression"
        } else {
            "CRAP ceiling"
        };
        let message = if has_ratchet {
            CRAP_REGRESSION_GH
        } else {
            CRAP_CEILING_GH
        };
        eprintln!("::error title={title}::{message}");
    }
    eprintln!("\n----------\n{hint}\n----------");
    format!("{error}\n\n{hint}").into()
}

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

fn run_cmd(
    log: &CiLog,
    program: impl AsRef<OsStr>,
    args: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let program = program.as_ref();
    let gate = begin_cmd(log, program, args)?;
    finish_cmd(log, gate, spawn_to_log(log, program, args))
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

fn spawn_to_log(
    log: &CiLog,
    program: &OsStr,
    args: &[&str],
) -> io::Result<std::process::ExitStatus> {
    let stdout = std::fs::OpenOptions::new().append(true).open(&log.path)?;
    let stderr = std::fs::OpenOptions::new().append(true).open(&log.path)?;
    std_command(program)
        .args(args)
        .current_dir(&log.root)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .env("CARGO_TERM_COLOR", "never")
        .status()
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
    let text = strip_ansi(output);
    let lines: Vec<&str> = text.lines().collect();
    let mut picked = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (idx, line) in lines.iter().enumerate() {
        if !is_ci_failure_line(line) {
            continue;
        }
        let start = idx;
        let end = extra_context_end(&lines, idx);
        for (i, candidate) in lines.iter().enumerate().take(end).skip(start) {
            if seen.insert(i) {
                picked.push((*candidate).to_string());
            }
        }
    }
    if picked.len() > EXTRACT_MAX_LINES {
        let more = picked.len() - EXTRACT_MAX_LINES;
        picked.truncate(EXTRACT_MAX_LINES);
        picked.push(format!("… {more} more matching lines in {CI_LOG_FILE}"));
    }
    picked.join("\n")
}

fn extra_context_end(lines: &[&str], anchor: usize) -> usize {
    let mut end = (anchor + 1).min(lines.len());
    while end < lines.len() && end < anchor + 8 {
        let trimmed = lines[end].trim_start();
        if trimmed.starts_with("-->") || trimmed.starts_with('|') || trimmed.starts_with("= ") {
            end += 1;
        } else {
            break;
        }
    }
    end
}

fn is_ci_failure_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    line.contains(" FAILED")
        || lower.contains("error[")
        || lower.contains("error:")
        || lower.contains("panicked at")
        || lower.contains("test result: failed")
        || lower.contains("could not compile")
        || lower.contains("regressed")
        || lower.contains("crap check failed")
        || (line.contains('┆') && line.contains('+') && !line.contains("NEW"))
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
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
mod tests {
    use super::{
        BASELINE_FILE, CI_LOG_FILE, CRAP_CEILING, CRAP_CEILING_GH, CRAP_CEILING_HINT,
        CRAP_COMPARE_SUMMARY, CRAP_EMPTY_BASELINE, CRAP_HOST_CEILING_ONLY, CRAP_REGRESSION_GH,
        CRAP_REGRESSION_HINT, CiLog, EXTRACT_MAX_LINES, LCOV_FILE, LLVM_COV_ARGS, StageOutcome,
        USAGE, announce_compare, baseline_has_entries, compare_args, compare_banners,
        crap_failure_hint, emit_crap_failure, exe_lives_in_cargo_target, extract_ci_failures, git,
        porcelain_path, relativize_json_file, relativize_lcov, repo_relative_source_path,
        repository_root, run_cmd, stage_ratcheted_baseline, strip_ansi, uses_per_function_ratchet,
    };
    use liberado_common::process::std_command;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn init_repo() -> tempfile::TempDir {
        let temp = tempdir().unwrap();
        let root = temp.path();
        assert!(
            std_command("git")
                .args(["init", "-q"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        for (key, value) in [
            ("user.email", "liberado@example.invalid"),
            ("user.name", "Liberado Test"),
        ] {
            assert!(
                std_command("git")
                    .args(["config", key, value])
                    .current_dir(root)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fs::write(root.join("README"), "base\n").unwrap();
        git(root, &["add", "README"]).unwrap();
        git(root, &["commit", "-q", "-m", "base"]).unwrap();
        temp
    }

    fn commit_contains(root: &Path, needle: &str) -> bool {
        git(root, &["show", "--name-only", "--pretty=format:", "HEAD"])
            .unwrap()
            .lines()
            .any(|line| line.trim() == needle)
    }

    #[test]
    fn finds_the_workspace_from_the_checkout_root() {
        let root = repository_root().expect("test runs from the workspace");
        assert!(root.join("Cargo.toml").is_file());
        assert!(root.join("crates").is_dir());
    }

    /// A program that does not exist surfaces as a start failure with the program named — the
    /// error the user needs when the preflight environment is missing a tool.
    #[test]
    fn run_reports_a_missing_program() {
        let temp = tempdir().unwrap();
        let log = CiLog::create(temp.path()).unwrap();
        let error = run_cmd(&log, "definitely-not-a-real-program-xyz", &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("could not start"), "{error}");
        assert!(
            error.contains("definitely-not-a-real-program-xyz"),
            "{error}"
        );
        assert!(error.contains(CI_LOG_FILE), "{error}");
        let logged = std::fs::read_to_string(&log.path).unwrap();
        assert!(
            logged.contains("definitely-not-a-real-program-xyz"),
            "{logged}"
        );
    }

    #[test]
    fn failing_cargo_command_surfaces_extracted_errors_and_the_log_path() {
        let temp = tempdir().unwrap();
        let log = CiLog::create(temp.path()).unwrap();
        let error = run_cmd(&log, "cargo", &["definitely-not-a-cargo-flag-xyz"])
            .unwrap_err()
            .to_string();
        assert!(error.contains(CI_LOG_FILE), "{error}");
        assert!(error.contains("error:"), "{error}");
        let logged = std::fs::read_to_string(&log.path).unwrap();
        assert!(logged.contains("error:"), "{logged}");
    }

    #[test]
    fn extract_ci_failures_names_tests_compiler_errors_and_crap() {
        let log = "\
Compiling liberado-notify v0.1.0
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.5s
running 19 tests
test tests::channel_name_is_telegram ... ok
test tests::from_env_reads_both_telegram_vars_and_default_base ... FAILED

thread 'tests::from_env_reads_both_telegram_vars_and_default_base' panicked at crates/notify/src/lib.rs:797:29:
both vars set -> Some

test result: FAILED. 17 passed; 1 failed; 2 ignored

error: test failed, to rerun pass `-p liberado-notify --lib`

error[E0425]: cannot find value `foo` in this scope
  --> crates/cli/src/ci_cmd.rs:123:5
   |
123 |     foo
    |     ^^^ not found in this scope
    = note: this error originates from a macro

↑ 1 regressed  ↓ 0 improved  ★ 0 new
│ ✓ ┆ 30.0 ┆ +18.0 ┆  5 ┆ compare_to_baseline
";
        let extracted = extract_ci_failures(log);
        assert!(
            extracted.contains("from_env_reads_both_telegram_vars_and_default_base ... FAILED"),
            "{extracted}"
        );
        assert!(extracted.contains("panicked at"), "{extracted}");
        assert!(extracted.contains("error[E0425]"), "{extracted}");
        assert!(
            extracted.contains("crates/cli/src/ci_cmd.rs:123:5"),
            "{extracted}"
        );
        assert!(
            extracted.contains("error: test failed, to rerun pass `-p liberado-notify --lib`"),
            "{extracted}"
        );
        assert!(extracted.contains("↑ 1 regressed"), "{extracted}");
        assert!(extracted.contains("compare_to_baseline"), "{extracted}");
        assert!(!extracted.contains("Compiling"), "{extracted}");
        assert!(
            !extracted.contains("channel_name_is_telegram ... ok"),
            "{extracted}"
        );
    }

    #[test]
    fn extract_ci_failures_caps_the_console_excerpt() {
        let mut log = String::new();
        for i in 0..(EXTRACT_MAX_LINES + 20) {
            log.push_str(&format!("error[E0001]: boom {i}\n"));
        }
        let extracted = extract_ci_failures(&log);
        let lines: Vec<_> = extracted.lines().collect();
        assert!(lines.len() <= EXTRACT_MAX_LINES + 1, "{}", lines.len());
        assert!(extracted.contains(CI_LOG_FILE), "{extracted}");
        assert!(extracted.contains("more matching lines"), "{extracted}");
    }

    #[test]
    fn strip_ansi_drops_color_codes_before_matching() {
        let colored = "\u{1b}[31merror[E0425]\u{1b}[0m: missing\n";
        assert_eq!(strip_ansi(colored), "error[E0425]: missing\n");
        let extracted = extract_ci_failures(colored);
        assert!(extracted.contains("error[E0425]"), "{extracted}");
    }

    #[test]
    fn announce_compare_records_the_empty_baseline_banner() {
        let temp = tempdir().unwrap();
        let log = CiLog::create(temp.path()).unwrap();
        assert!(!announce_compare(&log).unwrap());
        let text = std::fs::read_to_string(&log.path).unwrap();
        assert!(text.contains("no entries yet"), "{text}");
    }

    #[test]
    fn ci_log_create_truncates_a_previous_run() {
        let temp = tempdir().unwrap();
        let first = CiLog::create(temp.path()).unwrap();
        first.writeln("old run").unwrap();
        let second = CiLog::create(temp.path()).unwrap();
        let text = std::fs::read_to_string(&second.path).unwrap();
        assert!(!text.contains("old run"), "{text}");
        assert!(text.contains(CI_LOG_FILE), "{text}");
    }

    #[test]
    fn usage_names_the_three_verbs() {
        assert!(USAGE.contains("check"));
        assert!(USAGE.contains("crap"));
        assert!(USAGE.contains("ratchet"));
    }

    #[test]
    fn relativize_lcov_strips_the_workspace_root() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".liberado")).unwrap();
        let abs = root.join("crates").join("foo.rs");
        fs::write(
            root.join(LCOV_FILE),
            format!("SF:{}\nend_of_record\n", abs.display()),
        )
        .unwrap();
        relativize_lcov(root).unwrap();
        let text = fs::read_to_string(root.join(LCOV_FILE)).unwrap();
        assert_eq!(text.lines().next(), Some("SF:crates/foo.rs"));
    }

    #[test]
    fn relativize_json_file_rewrites_source_paths() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let abs = root.join("crates").join("foo.rs");
        let escaped = abs.display().to_string().replace('\\', "\\\\");
        fs::write(
            root.join("report.json"),
            format!(r#"{{"entries":[{{"file":"{escaped}"}}]}}"#),
        )
        .unwrap();
        relativize_json_file(root, "report.json").unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(root.join("report.json")).unwrap()).unwrap();
        assert_eq!(value["entries"][0]["file"], "crates/foo.rs");
    }

    #[test]
    fn repo_relative_path_drops_the_workspace_root_on_either_os() {
        let root = Path::new(if cfg!(windows) {
            r"C:\Users\Shiloh\Code\life-os"
        } else {
            "/home/runner/work/life-os/life-os"
        });
        let file = if cfg!(windows) {
            r"C:\Users\Shiloh\Code\life-os\crates\vault\src\lib.rs"
        } else {
            "/home/runner/work/life-os/life-os/crates/vault/src/lib.rs"
        };
        assert_eq!(
            repo_relative_source_path(root, file),
            "crates/vault/src/lib.rs"
        );
        assert_eq!(
            repo_relative_source_path(root, "crates/vault/src/lib.rs"),
            "crates/vault/src/lib.rs"
        );
    }

    #[test]
    fn llvm_cov_flags_are_not_test_binary_args() {
        assert!(LLVM_COV_ARGS.contains(&"--ignore-run-fail"));
        assert!(
            !LLVM_COV_ARGS.contains(&"--"),
            "a `--` would send llvm-cov flags to libtest, which rejects them"
        );
    }

    #[test]
    fn cargo_target_exe_is_the_image_cargo_test_would_overwrite() {
        assert!(exe_lives_in_cargo_target(Path::new(
            r"C:\repo\target\debug\liberado.exe"
        )));
        assert!(exe_lives_in_cargo_target(Path::new(
            "/repo/target/release/liberado"
        )));
        assert!(!exe_lives_in_cargo_target(Path::new(
            r"C:\Users\me\.cargo\bin\liberado.exe"
        )));
        assert!(!exe_lives_in_cargo_target(Path::new(
            "/repo/.liberado/liberado-ci"
        )));
    }

    #[test]
    fn regression_hint_tells_an_agent_not_to_raise_the_baseline() {
        assert!(CRAP_REGRESSION_HINT.contains("per-function"));
        assert!(CRAP_REGRESSION_HINT.contains("just ci"));
        assert!(CRAP_REGRESSION_HINT.contains("Do not raise the baseline"));
        assert!(CRAP_CEILING_HINT.contains(CRAP_CEILING));
        assert!(CRAP_REGRESSION_GH.contains("Ubuntu"));
        assert!(CRAP_CEILING_GH.contains(CRAP_CEILING));
        assert!(CRAP_HOST_CEILING_ONLY.contains("ceiling only"));
        assert_eq!(crap_failure_hint(true), CRAP_REGRESSION_HINT);
        assert_eq!(crap_failure_hint(false), CRAP_CEILING_HINT);
        let error = emit_crap_failure(true, "cargo crap failed".into()).to_string();
        assert!(error.contains("cargo crap failed"), "{error}");
        assert!(error.contains("Do not raise the baseline"), "{error}");
    }

    #[test]
    fn compare_args_always_enforce_the_150_ceiling() {
        let ceiling = compare_args(false);
        assert!(ceiling.contains(&"--fail-above"));
        assert!(ceiling.contains(&"--threshold"));
        assert!(ceiling.contains(&CRAP_CEILING));
        assert!(!ceiling.contains(&"--fail-regression"));
        let ratchet = compare_args(true);
        assert!(ratchet.contains(&"--fail-above"));
        assert!(ratchet.contains(&"--threshold"));
        assert!(ratchet.contains(&CRAP_CEILING));
        assert!(ratchet.contains(&"--fail-regression"));
        assert!(ratchet.contains(&"--baseline"));
    }

    /// A toml that still names a higher ceiling would let `cargo crap` (no flags)
    /// and this check disagree. The CI argv is explicit; the file must still
    /// match so a bare `cargo crap --fail-above` is the same gate.
    #[test]
    fn cargo_crap_toml_threshold_matches_the_ci_ceiling() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crate is crates/cli");
        let toml = std::fs::read_to_string(root.join(".cargo-crap.toml")).expect("toml");
        let expected = format!("threshold = {CRAP_CEILING}.0");
        assert!(
            toml.contains(&expected),
            ".cargo-crap.toml must set {expected}; got:\n{toml}"
        );
    }

    #[test]
    fn per_function_ratchet_runs_only_on_linux_with_a_filled_baseline() {
        assert!(!uses_per_function_ratchet(false));
        assert_eq!(
            uses_per_function_ratchet(true),
            cfg!(target_os = "linux"),
            "a filled baseline still does not run --fail-regression off Linux"
        );
        let args = compare_args(uses_per_function_ratchet(true));
        assert_eq!(
            args.contains(&"--fail-regression"),
            cfg!(target_os = "linux")
        );
    }

    #[test]
    fn compare_banners_name_the_host_rule() {
        let linux_filled = compare_banners(true, true);
        assert_eq!(linux_filled, vec![CRAP_COMPARE_SUMMARY]);
        let windows_filled = compare_banners(true, false);
        assert_eq!(
            windows_filled,
            vec![CRAP_HOST_CEILING_ONLY, CRAP_COMPARE_SUMMARY]
        );
        let empty = compare_banners(false, false);
        assert_eq!(empty, vec![CRAP_EMPTY_BASELINE, CRAP_COMPARE_SUMMARY]);
        let empty_linux = compare_banners(false, true);
        assert_eq!(empty_linux, vec![CRAP_EMPTY_BASELINE, CRAP_COMPARE_SUMMARY]);
    }

    #[test]
    fn empty_or_missing_baseline_is_not_a_ratchet_yet() {
        let temp = tempdir().unwrap();
        assert!(!baseline_has_entries(&temp.path().join("missing.json")));
        let empty = temp.path().join("empty.json");
        fs::write(&empty, r#"{"$schema":"x","version":"0.0.2","entries":[]}"#).unwrap();
        assert!(!baseline_has_entries(&empty));
        let filled = temp.path().join("filled.json");
        fs::write(
            &filled,
            r#"{"version":"0.0.2","entries":[{"function":"f","crap":1.0}]}"#,
        )
        .unwrap();
        assert!(baseline_has_entries(&filled));
    }

    #[test]
    fn porcelain_path_skips_the_two_status_columns() {
        assert_eq!(
            porcelain_path("M  crap-baseline.json"),
            Some("crap-baseline.json")
        );
        assert_eq!(porcelain_path("?? other.rs"), Some("other.rs"));
        assert_eq!(porcelain_path("M"), None);
    }

    #[test]
    fn a_clean_tree_amends_the_baseline_onto_head() {
        let temp = init_repo();
        let root = temp.path();
        fs::write(root.join(BASELINE_FILE), "{\"entries\":[]}\n").unwrap();
        assert_eq!(
            stage_ratcheted_baseline(root).unwrap(),
            StageOutcome::Amended
        );
        assert!(commit_contains(root, BASELINE_FILE));
        assert!(git(root, &["status", "--porcelain"]).unwrap().is_empty());
    }

    #[test]
    fn a_dirty_tree_only_stages_the_baseline() {
        let temp = init_repo();
        let root = temp.path();
        fs::write(root.join("dirty.rs"), "fn f() {}\n").unwrap();
        fs::write(root.join(BASELINE_FILE), "{\"entries\":[]}\n").unwrap();
        assert_eq!(
            stage_ratcheted_baseline(root).unwrap(),
            StageOutcome::Staged
        );
        assert!(!commit_contains(root, BASELINE_FILE));
        let status = git(root, &["status", "--porcelain"]).unwrap();
        assert!(
            status.lines().any(|line| line.contains(BASELINE_FILE)
                && line.as_bytes().first().is_some_and(|c| *c != b'?')),
            "baseline should be staged:\n{status}"
        );
        assert!(
            status.lines().any(|line| line.contains("dirty.rs")),
            "other dirty files stay unstaged:\n{status}"
        );
    }

    #[test]
    fn an_unchanged_baseline_is_a_no_op() {
        let temp = init_repo();
        let root = temp.path();
        fs::write(root.join(BASELINE_FILE), "{\"entries\":[]}\n").unwrap();
        git(root, &["add", BASELINE_FILE]).unwrap();
        git(root, &["commit", "-q", "-m", "baseline"]).unwrap();
        assert_eq!(
            stage_ratcheted_baseline(root).unwrap(),
            StageOutcome::Unchanged
        );
        assert_eq!(
            git(root, &["log", "-1", "--pretty=%s"]).unwrap().trim(),
            "baseline"
        );
    }
}
