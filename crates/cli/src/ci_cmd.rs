//! Repository checks that used to live in shell-specific preflight scripts.
//!
//! ## CRAP ratchet
//!
//! `crap-baseline.json` is the last best per-function score. GitHub only *reads* it
//! (`liberado ci crap` / `--fail-regression`). A local `liberado ci` run that stays
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
//! the 450 ceiling only (`--fail-above`). The per-function ratchet
//! (`--fail-regression`) runs on Linux, which is GitHub's Ubuntu job.
//!
//! `just ci` is `cargo run -p liberado-cli -- ci`. On Windows, `cargo test` cannot
//! overwrite a running `target/debug/liberado.exe` (Access is denied). A second
//! process still holds that path (`cargo run` waits on it). Before `cargo test`,
//! `liberado ci` *renames* the running image to `.liberado/liberado-ci` so cargo
//! can write a new artifact at the old path. Usage-only verbs do not move it.

use std::ffi::OsStr;
use std::io;
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
(per-function ratchet: 50 cannot become 60, even under the 450 ceiling). \
cargo-crap named the functions above. Split the function or add tests until \
each score is at or below its baseline. Do not raise the baseline. \
`just ci` will not rewrite it while this check is red. Fix locally, then push.";

/// Printed after `cargo crap` exits non-zero when the baseline is still empty.
const CRAP_CEILING_HINT: &str = "\
CRAP check failed. A function is above the 450 ceiling (`--fail-above`). \
Split it or add tests. New functions must land at or below 450.";

/// One-line GitHub Actions annotation (newlines are not legal in `::error`).
const CRAP_REGRESSION_GH: &str = "\
A function CRAP score went up vs crap-baseline.json (per-function ratchet). \
Split the function or add tests. Do not raise the baseline. \
Linux `just ci` or this Ubuntu job is the check that matches the file.";

/// Banner when this host is not Linux: do not run `--fail-regression` here.
const CRAP_HOST_CEILING_ONLY: &str = "\
[liberado ci] this host is not Linux — ceiling only (450). \
GitHub's Ubuntu job runs the per-function ratchet.";

const CRAP_CEILING_GH: &str = "\
A function is above the 450 CRAP ceiling. Split it or add tests. \
New functions must land at or below 450.";

/// Dispatch `liberado ci …`. No subcommand means the local full run (gates + ratchet).
pub fn run(args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args.peekable();
    match args.next().as_deref() {
        None => local_run(),
        Some("check") if args.peek().is_none() => check(),
        Some("crap") if args.peek().is_none() => crap_check(),
        Some("ratchet") if args.peek().is_none() => crap_ratchet(),
        _ => Err(USAGE.into()),
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
pub fn check() -> Result<(), Box<dyn std::error::Error>> {
    let root = repository_root()?;
    vacate_cargo_target_image()?;
    run_cmd(&root, "cargo", &["fmt", "--check"])?;
    run_cmd(
        &root,
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
    run_cmd(&root, "cargo", &["test", "--workspace", "--no-fail-fast"])?;
    run_cmd(&root, "cargo", &["deny", "check"])?;
    Ok(())
}

/// Full local CI: the ship preflight, then the CRAP check, then rewrite and stage the baseline.
pub fn local_run() -> Result<(), Box<dyn std::error::Error>> {
    check()?;
    crap_ratchet()
}

/// Compare the current tree against `crap-baseline.json`. Never writes the baseline.
///
/// Always writes `.liberado/crap-current.json` (gitignored) so a red GitHub job
/// still has an Ubuntu-shaped report to commit as the next baseline. Coverage is
/// host-sensitive; a Windows-generated file fails `--fail-regression` on Ubuntu.
pub fn crap_check() -> Result<(), Box<dyn std::error::Error>> {
    let root = repository_root()?;
    generate_lcov(&root)?;
    write_crap_json(&root, CURRENT_REPORT)?;
    compare_to_baseline(&root)
}

/// Check, then replace `crap-baseline.json` with this run's scores.
pub fn crap_ratchet() -> Result<(), Box<dyn std::error::Error>> {
    let root = repository_root()?;
    generate_lcov(&root)?;
    compare_to_baseline(&root)?;
    if !cfg!(target_os = "linux") {
        eprintln!(
            "[liberado ci] {BASELINE_FILE} write is Linux-only \
             (GitHub's Ubuntu job is the host of truth). Compared only."
        );
        return Ok(());
    }
    write_baseline(&root)?;
    match stage_ratcheted_baseline(&root)? {
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

fn generate_lcov(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    require_tool(
        root,
        "llvm-cov",
        &["cargo", "llvm-cov", "--version"],
        "cargo install cargo-llvm-cov --locked",
    )?;
    std::fs::create_dir_all(root.join(".liberado"))?;
    run_cmd(root, "cargo", LLVM_COV_ARGS)?;
    relativize_lcov(root)
}

fn compare_to_baseline(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    require_crap(root)?;
    let has_entries = baseline_has_entries(&root.join(BASELINE_FILE));
    let fail_regression = uses_per_function_ratchet(has_entries);
    if has_entries && !fail_regression {
        eprintln!("{CRAP_HOST_CEILING_ONLY}");
    } else if !has_entries {
        eprintln!(
            "[liberado ci] {BASELINE_FILE} has no entries yet — ceiling only (`--fail-above`). \
             A green Linux `liberado ci ratchet` fills the per-function ratchet."
        );
    }
    eprintln!(
        "[liberado ci] CRAP compare against {BASELINE_FILE} \
         (per-function ratchet on Linux; 450 is the new-function ceiling)"
    );
    run_cmd(root, "cargo", &compare_args(fail_regression))
        .map_err(|error| emit_crap_failure(fail_regression, error))
}

/// `--fail-regression` only on Linux. Coverage numbers are host-sensitive;
/// a Windows compare against the Ubuntu baseline false-fails (`explain_write` +127).
fn uses_per_function_ratchet(baseline_has_entries: bool) -> bool {
    baseline_has_entries && cfg!(target_os = "linux")
}

fn compare_args(fail_regression: bool) -> Vec<&'static str> {
    let mut args = vec!["crap", "--workspace", "--lcov", LCOV_FILE, "--fail-above"];
    if fail_regression {
        args.extend_from_slice(&["--baseline", BASELINE_FILE, "--fail-regression"]);
    }
    args
}

fn write_baseline(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    write_crap_json(root, BASELINE_FILE)
}

fn write_crap_json(root: &Path, output: &str) -> Result<(), Box<dyn std::error::Error>> {
    require_crap(root)?;
    run_cmd(
        root,
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
    relativize_json_file(root, output)
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
    root: &Path,
    program: impl AsRef<OsStr>,
    args: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let program = program.as_ref();
    let shown = program.to_string_lossy();
    eprintln!("[liberado ci] {shown} {}", args.join(" "));
    let status = std_command(program)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| {
            io::Error::new(error.kind(), format!("could not start {shown}: {error}"))
        })?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{shown} {} failed with {status}", args.join(" ")).into())
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
mod tests {
    use super::{
        BASELINE_FILE, CRAP_CEILING_GH, CRAP_CEILING_HINT, CRAP_HOST_CEILING_ONLY,
        CRAP_REGRESSION_GH, CRAP_REGRESSION_HINT, LCOV_FILE, LLVM_COV_ARGS, StageOutcome, USAGE,
        baseline_has_entries, compare_args, crap_failure_hint, emit_crap_failure,
        exe_lives_in_cargo_target, git, porcelain_path, relativize_json_file, relativize_lcov,
        repo_relative_source_path, repository_root, run_cmd, stage_ratcheted_baseline,
        uses_per_function_ratchet,
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
        let root = repository_root().unwrap();
        let error = run_cmd(&root, "definitely-not-a-real-program-xyz", &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("could not start"), "{error}");
        assert!(
            error.contains("definitely-not-a-real-program-xyz"),
            "{error}"
        );
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
        assert!(CRAP_CEILING_HINT.contains("450"));
        assert!(CRAP_REGRESSION_GH.contains("Ubuntu"));
        assert!(CRAP_CEILING_GH.contains("450"));
        assert!(CRAP_HOST_CEILING_ONLY.contains("ceiling only"));
        assert_eq!(crap_failure_hint(true), CRAP_REGRESSION_HINT);
        assert_eq!(crap_failure_hint(false), CRAP_CEILING_HINT);
        let error = emit_crap_failure(true, "cargo crap failed".into()).to_string();
        assert!(error.contains("cargo crap failed"), "{error}");
        assert!(error.contains("Do not raise the baseline"), "{error}");
    }

    #[test]
    fn compare_args_always_enforce_the_450_ceiling() {
        let ceiling = compare_args(false);
        assert!(ceiling.contains(&"--fail-above"));
        assert!(!ceiling.contains(&"--fail-regression"));
        let ratchet = compare_args(true);
        assert!(ratchet.contains(&"--fail-above"));
        assert!(ratchet.contains(&"--fail-regression"));
        assert!(ratchet.contains(&"--baseline"));
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
