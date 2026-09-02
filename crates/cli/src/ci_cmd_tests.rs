//! Split from `ci_cmd.rs` for module-health boundaries.

use super::new_function_ceiling::compare_args;
use super::{
    BASELINE_FILE, CI_LOG_FILE, CRAP_CEILING, CRAP_CEILING_GH, CRAP_CEILING_HINT,
    CRAP_COMPARE_SUMMARY, CRAP_EMPTY_BASELINE, CRAP_HOST_CEILING_ONLY, CRAP_REGRESSION_GH,
    CRAP_REGRESSION_HINT, CRAP_REGRESSION_MIN, CRAP_REPORT_ARGS, CRAP_REPORT_THRESHOLD, CiLog,
    EXTRACT_MAX_LINES, LCOV_FILE, LLVM_COV_ARGS, StageOutcome, announce_compare,
    baseline_has_entries, compare_banners, compare_to_baseline, crap_failure_hint,
    emit_crap_failure, exe_lives_in_cargo_target, extract_ci_failures, git, porcelain_path,
    relativize_json_file, relativize_lcov, repo_relative_source_path, repository_root, run_cmd,
    stage_ratcheted_baseline, uses_per_function_ratchet,
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

/// A baseline compare of an empty workspace cannot succeed: either the cargo-crap probe
/// fails (tool absent) or the comparer has no usable coverage input. A `compare_to_baseline`
/// body replaced by `Ok(())` (the surviving mutant) reports success from thin air.
#[test]
fn compare_to_baseline_never_succeeds_on_an_empty_workspace() {
    let temp = tempdir().unwrap();
    let log = CiLog::create(temp.path()).unwrap();
    assert!(
        compare_to_baseline(&log).is_err(),
        "comparing an empty workspace must be an error"
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
fn regression_hint_tells_an_agent_not_to_raise_the_baseline() {
    assert!(CRAP_REGRESSION_HINT.contains("per-function"));
    assert!(CRAP_REGRESSION_HINT.contains("just ci"));
    assert!(CRAP_REGRESSION_HINT.contains("below 10"));
    assert!(CRAP_REGRESSION_HINT.contains("Do not raise the baseline"));
    assert!(CRAP_REGRESSION_GH.contains("below 10"));
    assert!(CRAP_COMPARE_SUMMARY.contains("below 10"));
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
fn compare_args_apply_fail_above_only_when_the_baseline_is_empty() {
    let ceiling = compare_args(false, true);
    assert!(ceiling.contains(&"--fail-above"));
    assert!(ceiling.contains(&"--threshold"));
    assert!(ceiling.contains(&CRAP_CEILING));
    assert!(!ceiling.contains(&"--fail-regression"));
    assert!(
        !ceiling.contains(&"--min"),
        "ceiling-only must not hide low scores from a later reader of the argv"
    );
    let ratchet = compare_args(true, false);
    assert!(
        !ratchet.contains(&"--fail-above"),
        "a filled baseline has entries above the new-function ceiling; --fail-above would fail them"
    );
    assert!(ratchet.contains(&"--fail-regression"));
    assert!(ratchet.contains(&"--baseline"));
    assert!(ratchet.contains(&"--min"));
    assert!(ratchet.contains(&CRAP_REGRESSION_MIN));
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

#[test]
fn regression_compare_drops_current_scores_below_ten() {
    assert_eq!(CRAP_REGRESSION_MIN, "10");
    let ratchet = compare_args(true, false);
    let min_at = ratchet
        .iter()
        .position(|&flag| flag == "--min")
        .expect("--min is part of the ratchet argv");
    assert_eq!(ratchet.get(min_at + 1), Some(&CRAP_REGRESSION_MIN));
    assert!(
        ratchet.contains(&"--fail-regression"),
        "the floor only applies when the per-function detector runs"
    );
}

/// The toml names the new-function ceiling. `fail-above` stays off there because
/// existing baseline entries sit above 30; Liberado applies the ceiling itself.
#[test]
fn cargo_crap_toml_threshold_matches_the_ci_ceiling() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate is crates/cli");
    let toml = std::fs::read_to_string(root.join(".cargo-crap.toml")).expect("toml");
    let expected = format!("threshold = {CRAP_CEILING}");
    assert!(
        toml.lines().any(|line| line.trim() == expected),
        ".cargo-crap.toml must set {expected}; got:\n{toml}"
    );
    assert!(
        !toml
            .lines()
            .any(|line| line.trim_start().starts_with("min")),
        "min in .cargo-crap.toml would also filter baseline writes; got:\n{toml}"
    );
    assert!(
        toml.lines().any(|line| line.trim() == "fail-above = false"),
        "fail-above must stay off: existing functions sit above the new-function ceiling; got:\n{toml}"
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
    let args = compare_args(uses_per_function_ratchet(true), false);
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
