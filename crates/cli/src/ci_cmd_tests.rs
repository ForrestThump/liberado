//! Split from `ci_cmd.rs` for module-health boundaries.

use super::{
    CI_LOG_FILE, CRAP_REPORT_ARGS, CRAP_REPORT_THRESHOLD, CiLog, EXTRACT_MAX_LINES, LCOV_FILE,
    LLVM_COV_ARGS, exe_lives_in_cargo_target, extract_ci_failures, relativize_json_file,
    relativize_lcov, repo_relative_source_path, repository_root, run_cmd,
};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

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
fn extract_ci_failures_strips_color_codes_before_matching() {
    let colored = "\u{1b}[31merror[E0425]\u{1b}[0m: missing\n";
    let extracted = extract_ci_failures(colored);
    assert!(extracted.contains("error[E0425]"), "{extracted}");
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
    assert!(text.contains("cargo jobs="), "{text}");
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
fn configured_cargo_target_exe_is_the_image_cargo_test_would_overwrite() {
    assert!(exe_lives_in_cargo_target(Path::new(
        r"C:\repo\target-windows-final\debug\liberado.exe"
    )));
}

#[test]
fn local_ci_wires_the_base_aware_docs_impact_gate() {
    assert!(
        include_str!("ci_cmd/dispatch.rs")
            .contains("crate::readiness_cmd::audit_docs(&log.root)?;")
    );
}

#[test]
fn report_generation_defers_policy_to_the_explicit_compare() {
    assert!(
        CRAP_REPORT_THRESHOLD.parse::<f64>().unwrap() > 1e100,
        "report generation needs an effectively unreachable threshold"
    );
    let threshold_at = CRAP_REPORT_ARGS
        .iter()
        .position(|&flag| flag == "--threshold")
        .expect("report generation must override the configured fail-above threshold");
    assert_eq!(
        CRAP_REPORT_ARGS.get(threshold_at + 1),
        Some(&CRAP_REPORT_THRESHOLD)
    );
    assert!(!CRAP_REPORT_ARGS.contains(&"--fail-regression"));
    assert!(!CRAP_REPORT_ARGS.contains(&"--fail-above"));
    assert_eq!(CRAP_REPORT_ARGS.last(), Some(&"--output"));
}

/// The toml names the new-function CRAP limit. The diff gate reads it. `fail-above`
/// stays off so a bare `cargo crap` does not fail functions the diff did not touch.
#[test]
fn cargo_crap_toml_threshold_matches_the_diff_gate_limit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate is crates/cli");
    let toml = std::fs::read_to_string(root.join(".cargo-crap.toml")).expect("toml");
    assert!(
        toml.lines().any(|line| line.trim() == "threshold = 29.9"),
        ".cargo-crap.toml must set threshold = 29.9; got:\n{toml}"
    );
    assert!(
        !toml
            .lines()
            .any(|line| line.trim_start().starts_with("min")),
        "min in .cargo-crap.toml is not used by the diff gate; got:\n{toml}"
    );
    assert!(
        toml.lines().any(|line| line.trim() == "fail-above = false"),
        "fail-above must stay off; the diff gate owns pass/fail; got:\n{toml}"
    );
}
