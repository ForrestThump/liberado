//! Repository checks that used to live in shell-specific preflight scripts.
//!
//! ## Diff gate
//!
//! `liberado ci crap` and `liberado ci complexity` score only functions the git diff
//! touched since the merge-base. New functions must meet the config limits. A function
//! that already exists on the merge-base may stay over a limit, and that score must not
//! rise. There is no committed per-function baseline and no waiver. See
//! [`diff_gate`] for the rename, copy, and split rules.
//!
//! The same host measures both sides, so Windows and Linux both apply the full CRAP rule.
//! Module-health and unwrap baselines stay a Linux-only write.
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
//!
//! Workspace Clippy and llvm-cov start several rustc processes. Each often needs
//! several GiB. `just ci` is `cargo run … -- ci`, so those children inherit a
//! jobserver sized to the CPU count. `liberado ci` drops that jobserver and sets
//! `CARGO_BUILD_JOBS` from free RAM (4 GiB reserve, 4 GiB per rustc). Override
//! with `LIBERADO_CI_JOBS`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use liberado_common::process::std_command;
use serde_json::Value;

/// Pinned so a cargo-crap release cannot silently reshape the report schema.
const CARGO_CRAP_VERSION: &str = "0.4.3";
const LCOV_FILE: &str = ".liberado/crap.lcov";
const CURRENT_REPORT: &str = ".liberado/crap-current.json";
const USAGE: &str = "usage: liberado ci [--base REV] [check|crap|crap-linux|modules|modules-ratchet|complexity|unwraps|unwraps-ratchet|ready|verify-ready]";
const VACATED_BIN: &str = "liberado-ci";
const CI_LOG_FILE: &str = ".liberado/ci.log";
const EXTRACT_MAX_LINES: usize = liberado_coder_core::FAILURE_EXTRACT_MAX_LINES;

/// Report generation must not enforce `.cargo-crap.toml`'s `fail-above` policy. The diff
/// gate owns pass/fail. An unreachable threshold overrides the configured ceiling here.
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
/// job already owns pass/fail).
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

mod coverage_tools;
pub(crate) mod diff_gate;
mod log;
#[cfg(test)]
pub(crate) use log::extract_ci_failures;
pub(crate) use log::{CiLog, run_cmd};
/// Dispatch `liberado ci …`. No subcommand means the local full run.
mod dispatch;
mod job_budget;
mod runtime_support;

pub use dispatch::run;
use runtime_support::{move_running_image, vacated_image_destination};

fn with_log(
    body: impl FnOnce(&CiLog) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    body(&CiLog::create(&repository_root()?)?)
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

/// Score changed functions for CRAP. Writes `.liberado/crap-current.json` and the diff-gate report.
fn crap_check(log: &CiLog, base: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    diff_gate::check_crap(log, base)
}

pub(crate) fn crap_for_root(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    crap_check(&CiLog::create(root)?, None)
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
    relativize_lcov(&log.root, &log.root.join(LCOV_FILE))
}

pub(super) fn write_crap_json(log: &CiLog, output: &str) -> Result<(), Box<dyn std::error::Error>> {
    require_crap(&log.root)?;
    let mut args = CRAP_REPORT_ARGS.to_vec();
    args.push(output);
    run_cmd(log, "cargo", &args)?;
    relativize_json_file(&log.root, output)
}

/// Strip the workspace root from an LCOV `SF:` path so Ubuntu CI and a Windows
/// `just ci` compare the same keys (`crates/foo.rs`, not `C:\Users\...\foo.rs`).
fn relativize_lcov(root: &Path, lcov_file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(lcov_file)?;
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
    std::fs::write(lcov_file, out)?;
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
