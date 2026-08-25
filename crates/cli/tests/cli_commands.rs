use liberado_common::process::std_command;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn run_cli(cwd: &Path, args: &[&str]) -> std::process::Output {
    std_command(env!("CARGO_BIN_EXE_liberado"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("liberado CLI should start")
}

/// Run the CLI with an empty `LIBERADO_CONFIG_DIR` so resolution cannot walk up from the
/// test binary into this checkout's `config/`. The empty dir is kept for the child process only
/// (set on the command, not in the test process env).
fn run_cli_no_config(cwd: &Path, args: &[&str]) -> std::process::Output {
    let config_dir = tempdir().expect("empty config dir");
    std_command(env!("CARGO_BIN_EXE_liberado"))
        .env("LIBERADO_CONFIG_DIR", config_dir.path())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("liberado CLI should start")
}

#[test]
fn docs_site_command_generates_searchable_catalog_and_mirrored_pages() {
    let temp = tempdir().expect("temporary repository");
    let root = temp.path();
    fs::create_dir(root.join("crates")).expect("crates directory");
    let docs_dir = root.join(["do", "cs"].concat());
    fs::create_dir(&docs_dir).expect("docs directory");
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("Cargo.toml");
    fs::write(
        docs_dir.join("index.md"),
        "---\nkind: index\nstatus: active\nauthority: advisory\n---\n# Index\n\n[Guide](guide.md)\n",
    )
    .expect("index document");
    fs::write(docs_dir.join("guide.md"), "# Guide\n\nUseful text.\n").expect("guide document");
    let out = root.join("generated");

    let output = run_cli(
        root,
        &[
            "docs",
            "site",
            "--root",
            root.to_str().expect("UTF-8 temp path"),
            "--out",
            out.to_str().expect("UTF-8 output path"),
        ],
    );

    assert!(
        output.status.success(),
        "docs site failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out.join("index.html").is_file());
    assert!(out.join("SUMMARY.md").is_file());
    assert_eq!(
        fs::read_to_string(
            out.join("pages")
                .join(docs_dir.file_name().unwrap())
                .join("guide.md")
        )
        .expect("mirrored guide"),
        "# Guide\n\nUseful text.\n"
    );

    let search: Value = serde_json::from_str(
        &fs::read_to_string(out.join("search-index.json")).expect("search index"),
    )
    .expect("valid search index JSON");
    assert_eq!(search["documents"].as_array().expect("documents").len(), 2);
    let docs_prefix = ["do", "cs"].concat();
    assert_eq!(
        search["backlinks"][format!("{docs_prefix}/guide.md")][0],
        format!("{docs_prefix}/index.md")
    );
    assert!(
        fs::read_to_string(out.join("index.html"))
            .expect("generated HTML")
            .contains("const BACKLINKS")
    );
}

#[test]
fn docs_link_check_command_uses_the_current_working_repository() {
    let temp = tempdir().expect("temporary repository");
    let root = temp.path();
    let docs_dir = root.join(["do", "cs"].concat());
    let docs_prefix = ["do", "cs"].concat();
    fs::create_dir_all(&docs_dir).expect("docs directory");
    fs::create_dir(root.join("crates")).expect("crates directory");
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("Cargo.toml");
    fs::write(
        root.join("README.md"),
        format!("[document]({docs_prefix}/guide.md)\n"),
    )
    .expect("README");
    fs::write(docs_dir.join("guide.md"), "# Guide\n").expect("guide document");

    let output = run_cli(root, &["docs", "check-links"]);

    assert!(
        output.status.success(),
        "docs link check failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PASS: all 1 link(s) resolve."));
}

// ── main.rs argument dispatch ───────────────────────────────────────────
//
// The single entry-point matchup prints a usage line and fails on a missing or unknown subcommand.
// These pin the user-facing contract without needing a daemon — the bad-arg branches return before
// any repository or network work. Run from a throwaway cwd so nothing depends on the checkout.

fn run_usage(cwd: &Path, args: &[&str]) -> String {
    let output = run_cli(cwd, args);
    assert!(
        !output.status.success(),
        "expected {args:?} to fail (usage)",
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn docs_requires_a_subcommand() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["docs"]);
    assert!(stderr.contains("usage: liberado docs"), "{stderr}");
}

#[test]
fn docs_crate_map_rejects_an_unknown_flag() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["docs", "crate-map", "--bogus"]);
    assert!(
        stderr.contains("usage: liberado docs crate-map"),
        "{stderr}"
    );
}

#[test]
fn docs_metadata_requires_a_subcommand() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["docs", "metadata"]);
    assert!(stderr.contains("usage: liberado docs metadata"), "{stderr}");
}

#[test]
fn docs_metadata_rejects_extra_arguments() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["docs", "metadata", "lint", "extra"]);
    assert!(stderr.contains("usage: liberado docs metadata"), "{stderr}");
}

#[test]
fn docs_audit_rejects_extra_arguments() {
    let temp = tempdir().unwrap();
    fs::create_dir(temp.path().join("crates")).unwrap();
    fs::write(
        temp.path().join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .unwrap();
    let stderr = run_usage(temp.path(), &["docs", "audit", "extra"]);
    assert!(stderr.contains("usage: liberado docs audit"), "{stderr}");
}

#[test]
fn ci_rejects_an_unknown_subcommand() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["ci", "bogus"]);
    assert!(stderr.contains("usage: liberado ci"), "{stderr}");
}

#[test]
fn config_requires_a_subcommand() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["config"]);
    assert!(stderr.contains("usage: liberado config"), "{stderr}");
}

#[test]
fn config_explain_requires_three_arguments() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["config", "explain", "only-one"]);
    assert!(
        stderr.contains("usage: liberado config explain"),
        "{stderr}"
    );
}

#[test]
fn shepherd_requires_a_mode_flag() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["shepherd"]);
    assert!(stderr.contains("usage: liberado shepherd"), "{stderr}");
}

/// `config check` parses as its own arm (not the catch-all usage error) and runs the loader;
/// from a throwaway directory it fails on the missing vault_path — a usage error instead would
/// mean the `Some("check")` arm was dropped.
#[test]
fn config_check_runs_the_loader() {
    let temp = tempdir().unwrap();
    let output = run_cli_no_config(temp.path(), &["config", "check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("usage: liberado config"),
        "the loader must run, not the usage catch-all, got: {stderr}"
    );
    assert!(
        stderr.contains("vault_path"),
        "expected a loader error about the missing vault, got: {stderr}"
    );
}

/// `docs crate-map` with no flag takes the "read" arm and only then fails on the missing
/// repository — a usage error instead would mean the `[]` arm was dropped.
#[test]
fn docs_crate_map_reads_by_default() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["docs", "crate-map"]);
    assert!(
        stderr.contains("could not find repository root"),
        "expected the repository-root error, got: {stderr}"
    );
}

/// `config explain` with all three arguments reaches the explain arm, not the usage error.
#[test]
fn config_explain_reaches_the_explain_arm() {
    let temp = tempdir().unwrap();
    let output = run_cli_no_config(
        temp.path(),
        &[
            "config",
            "explain",
            "dispatcher",
            "turbovault:write_note",
            "Learning/x.md",
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("usage: liberado config explain"),
        "the explain arm must run, got: {stderr}"
    );
    // Without a vault the explain guard fails on the first requirement — that error is proof the
    // explain path ran (any usage fallback would print a different string).
    assert!(
        stderr.contains("topology.vault_path is required"),
        "explain must reach the write guard, got: {stderr}"
    );
}

/// `shepherd config check` resolves the repository and prints the effective configuration — a
/// no-op instead would exit zero from a throwaway directory.
#[test]
fn shepherd_config_check_requires_a_repository() {
    let temp = tempdir().unwrap();
    let output = run_cli(temp.path(), &["shepherd", "config", "check"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("usage: liberado shepherd"),
        "the config-check arm must run, got: {stderr}"
    );
    assert!(
        stderr.contains("could not find repository root")
            || stderr.contains("cannot load topology"),
        "expected a repository/config error, got: {stderr}"
    );
}

#[test]
fn coder_requires_a_subcommand() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["coder"]);
    assert!(stderr.contains("usage:"), "{stderr}");
}

#[test]
fn coder_rejects_an_unknown_subcommand() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["coder", "bogus"]);
    assert!(stderr.contains("unknown coder subcommand"), "{stderr}");
}

/// A bare `liberado` (no subcommand) with no `LIBERADO_VAULT` set prints the usage line rather
/// than guessing a vault and launching a daemon.
#[test]
fn bare_invocation_requires_a_vault() {
    let output = std_command(env!("CARGO_BIN_EXE_liberado"))
        .env_remove("LIBERADO_VAULT")
        .current_dir(tempdir().unwrap().path())
        .output()
        .expect("liberado CLI should start");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("usage: liberado"), "{stderr}");
}

#[test]
fn compare_prepare_creates_pinned_worktrees_with_separate_caches_and_artifacts() {
    let source = tempdir().expect("temporary source repository");
    let runs = tempdir().expect("temporary run parent");
    initialize_compare_source(source.path());
    let run = runs.path().join("compare-17");

    let output = run_cli(
        source.path(),
        &[
            "coder",
            "compare",
            "prepare",
            run.to_str().expect("UTF-8 run path"),
            "--source",
            source.path().to_str().expect("UTF-8 source path"),
            "--commit",
            "main",
            "--compile-timeout-secs",
            "2400",
        ],
    );

    assert!(
        output.status.success(),
        "prepare failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(run.join("manifest.json")).expect("comparison manifest"))
            .expect("valid comparison manifest");
    assert_eq!(manifest["base_revision"], "main");
    assert_eq!(manifest["compile_timeout_secs"], 2400);
    let liberado_target = manifest["harnesses"]["liberado"]["target_dir"]
        .as_str()
        .expect("Liberado target path");
    let pi_target = manifest["harnesses"]["pi"]["target_dir"]
        .as_str()
        .expect("Pi target path");
    assert_ne!(
        liberado_target, pi_target,
        "build caches must not be shared"
    );
    for harness in ["liberado", "pi"] {
        let worktree = run.join("worktrees").join(harness);
        assert_eq!(
            git_capture_test(&worktree, &["rev-parse", "HEAD"]),
            manifest["base_commit"].as_str().expect("base commit")
        );
        assert!(worktree.join("turbovault").join("copied.txt").is_file());
        assert!(worktree.join("turbomcp").join("copied.txt").is_file());
        assert!(run.join("artifacts").join(harness).join("traces").is_dir());
        assert!(
            run.join("artifacts")
                .join(harness)
                .join("sessions")
                .is_dir()
        );
    }

    remove_compare_worktrees(source.path(), &run);
}

#[test]
fn compare_save_commits_failed_work_and_collects_git_and_trace_artifacts() {
    let source = tempdir().expect("temporary source repository");
    let runs = tempdir().expect("temporary run parent");
    initialize_compare_source(source.path());
    let run = runs.path().join("compare-save");
    let prepare = run_cli(
        source.path(),
        &[
            "coder",
            "compare",
            "prepare",
            run.to_str().expect("UTF-8 run path"),
            "--source",
            source.path().to_str().expect("UTF-8 source path"),
            "--commit",
            "main",
        ],
    );
    assert!(prepare.status.success());

    let pi = run.join("worktrees").join("pi");
    fs::write(pi.join("tracked.txt"), "agent result\n").expect("dirty Pi result");
    let trace_dir = pi.join("coder-traces");
    fs::create_dir(&trace_dir).expect("trace directory");
    fs::write(trace_dir.join("save-case.events.jsonl"), "{}\n").expect("trace");

    let save = run_cli(
        source.path(),
        &[
            "coder",
            "compare",
            "save",
            run.to_str().expect("UTF-8 run path"),
            "pi",
            "--session-id",
            "save-case",
            "--exit-code",
            "17",
            "--verifier-exit-code",
            "19",
        ],
    );
    assert!(
        save.status.success(),
        "save failed:\n{}\n{}",
        String::from_utf8_lossy(&save.stdout),
        String::from_utf8_lossy(&save.stderr)
    );
    assert_eq!(git_capture_test(&pi, &["status", "--short"]), "");
    assert_eq!(
        git_capture_test(&pi, &["show", "HEAD:tracked.txt"]),
        "agent result"
    );

    let artifacts = run.join("artifacts").join("pi");
    let result: serde_json::Value =
        serde_json::from_slice(&fs::read(artifacts.join("result.json")).expect("saved result"))
            .expect("valid saved result");
    assert_eq!(result["exit_code"], 17);
    assert_eq!(result["verifier_exit_code"], 19);
    assert_eq!(result["had_uncommitted_changes"], true);
    assert!(artifacts.join("git").join("diff.patch").is_file());
    assert!(
        artifacts
            .join("traces")
            .join("save-case.events.jsonl")
            .is_file()
    );
    let branch = result["archive_branch"].as_str().expect("archive branch");
    assert_eq!(
        git_capture_test(source.path(), &["rev-parse", branch]),
        git_capture_test(&pi, &["rev-parse", "HEAD"])
    );

    remove_compare_worktrees(source.path(), &run);
}

#[test]
fn compare_run_uses_owned_paths_and_saves_both_results() {
    let source = tempdir().expect("temporary source repository");
    let runs = tempdir().expect("temporary run parent");
    initialize_compare_source(source.path());
    let run = runs.path().join("compare-run");
    let prepare = run_cli(
        source.path(),
        &[
            "coder",
            "compare",
            "prepare",
            run.to_str().expect("UTF-8 run path"),
            "--source",
            source.path().to_str().expect("UTF-8 source path"),
            "--commit",
            "main",
        ],
    );
    assert!(prepare.status.success());
    let task = runs.path().join("task.txt");
    fs::write(&task, "task").expect("task file");
    let fake = write_fake_harness(runs.path());

    let output = std_command(env!("CARGO_BIN_EXE_liberado"))
        .args([
            "coder",
            "compare",
            "run",
            run.to_str().expect("UTF-8 run path"),
            "--task",
            task.to_str().expect("UTF-8 task path"),
            "--api-key-env",
            "LIBERADO_COMPARE_TEST_KEY",
            "--task-aware-context",
            "--liberado-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
            "--pi-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
        ])
        .env("LIBERADO_COMPARE_TEST_KEY", "test-only")
        .current_dir(source.path())
        .output()
        .expect("comparison run starts");
    assert!(
        output.status.success(),
        "run failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    for harness in ["liberado", "pi"] {
        let worktree = run.join("worktrees").join(harness);
        let artifacts = run.join("artifacts").join(harness);
        assert_eq!(git_capture_test(&worktree, &["status", "--short"]), "");
        assert_eq!(
            git_capture_test(&worktree, &["show", "HEAD:tracked.txt"]),
            "fake result"
        );
        assert!(artifacts.join("session.stdout.log").is_file());
        assert!(artifacts.join("session.stderr.log").is_file());
        assert!(artifacts.join("warmup.stdout.log").is_file());
        assert!(artifacts.join("warmup.stderr.log").is_file());
        assert!(artifacts.join("verifier.stdout.log").is_file());
        assert!(artifacts.join("verifier.stderr.log").is_file());
        assert!(artifacts.join("verifier-status.txt").is_file());
        assert!(artifacts.join("git").join("diff.patch").is_file());
        let result: serde_json::Value =
            serde_json::from_slice(&fs::read(artifacts.join("result.json")).expect("saved result"))
                .expect("valid saved result");
        assert_eq!(result["verifier_exit_code"], 0);
    }
    assert!(
        run.join("artifacts")
            .join("liberado")
            .join("traces")
            .join("compare-run-liberado.fake.json")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(run.join("task.txt")).expect("captured task"),
        "task"
    );
    let tuning = fs::read_to_string(run.join("config").join("tuning.toml"))
        .expect("captured comparison tuning");
    assert!(tuning.contains("timeout_secs = 1800"));
    assert!(tuning.contains("warmup_timeout_secs = 1800"));
    assert!(tuning.contains("warmup = false"));
    assert!(tuning.contains("[coder.repo_map]"));
    assert!(tuning.contains("task_aware = true"));
    let pins = fs::read_to_string(run.join("pins.txt")).expect("captured comparison pins");
    assert!(pins.contains("task_aware_context=true"));
    assert!(!pins.contains("write_scope_"));

    remove_compare_worktrees(source.path(), &run);
}

#[test]
fn compare_run_does_not_enforce_native_dispatch_scope() {
    let source = tempdir().expect("temporary source repository");
    let runs = tempdir().expect("temporary run parent");
    initialize_compare_source(source.path());
    let run = runs.path().join("compare-no-change-scope");
    let prepare = run_cli(
        source.path(),
        &[
            "coder",
            "compare",
            "prepare",
            run.to_str().expect("UTF-8 run path"),
            "--source",
            source.path().to_str().expect("UTF-8 source path"),
            "--commit",
            "main",
        ],
    );
    assert!(prepare.status.success());
    let task = runs.path().join("task.txt");
    fs::write(&task, "task").expect("task file");
    let fake = write_fake_harness(runs.path());

    let output = std_command(env!("CARGO_BIN_EXE_liberado"))
        .args([
            "coder",
            "compare",
            "run",
            run.to_str().expect("UTF-8 run path"),
            "--task",
            task.to_str().expect("UTF-8 task path"),
            "--api-key-env",
            "LIBERADO_COMPARE_TEST_KEY",
            "--liberado-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
            "--pi-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
        ])
        .env("LIBERADO_COMPARE_TEST_KEY", "test-only")
        .current_dir(source.path())
        .output()
        .expect("comparison run starts");
    assert!(
        output.status.success(),
        "comparison must not impose a native dispatch scope: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    for harness in ["liberado", "pi"] {
        let artifacts = run.join("artifacts").join(harness);
        let result: serde_json::Value =
            serde_json::from_slice(&fs::read(artifacts.join("result.json")).expect("saved result"))
                .expect("valid saved result");
        assert_eq!(result["verifier_exit_code"], 0);
    }

    remove_compare_worktrees(source.path(), &run);
}

#[test]
fn compare_run_saves_launch_failure_and_still_runs_the_other_harness() {
    let source = tempdir().expect("temporary source repository");
    let runs = tempdir().expect("temporary run parent");
    initialize_compare_source(source.path());
    let run = runs.path().join("compare-launch-failure");
    let prepare = run_cli(
        source.path(),
        &[
            "coder",
            "compare",
            "prepare",
            run.to_str().expect("UTF-8 run path"),
            "--source",
            source.path().to_str().expect("UTF-8 source path"),
            "--commit",
            "main",
        ],
    );
    assert!(prepare.status.success());
    let task = runs.path().join("task.txt");
    fs::write(&task, "task").expect("task file");
    let fake = write_fake_harness(runs.path());
    let missing = runs.path().join("missing-liberado-runner");

    let output = std_command(env!("CARGO_BIN_EXE_liberado"))
        .args([
            "coder",
            "compare",
            "run",
            run.to_str().expect("UTF-8 run path"),
            "--task",
            task.to_str().expect("UTF-8 task path"),
            "--api-key-env",
            "LIBERADO_COMPARE_TEST_KEY",
            "--liberado-bin",
            missing.to_str().expect("UTF-8 missing path"),
            "--pi-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
        ])
        .env("LIBERADO_COMPARE_TEST_KEY", "test-only")
        .current_dir(source.path())
        .output()
        .expect("comparison run starts");
    assert!(
        !output.status.success(),
        "one failed harness must fail the command"
    );

    let liberado_artifacts = run.join("artifacts").join("liberado");
    let pi_artifacts = run.join("artifacts").join("pi");
    assert!(liberado_artifacts.join("launch-error.txt").is_file());
    let liberado_result: serde_json::Value = serde_json::from_slice(
        &fs::read(liberado_artifacts.join("result.json")).expect("Liberado result"),
    )
    .expect("valid Liberado result");
    let pi_result: serde_json::Value =
        serde_json::from_slice(&fs::read(pi_artifacts.join("result.json")).expect("Pi result"))
            .expect("valid Pi result");
    assert_eq!(liberado_result["exit_code"], 127);
    assert_eq!(pi_result["exit_code"], 0);
    assert_eq!(
        git_capture_test(
            &run.join("worktrees").join("pi"),
            &["show", "HEAD:tracked.txt"]
        ),
        "fake result"
    );
    let tuning = fs::read_to_string(run.join("config").join("tuning.toml"))
        .expect("captured comparison tuning");
    assert!(!tuning.contains("task_aware"), "{tuning}");
    let pins = fs::read_to_string(run.join("pins.txt")).expect("captured comparison pins");
    assert!(pins.contains("task_aware_context=false"), "{pins}");

    remove_compare_worktrees(source.path(), &run);
}

#[test]
fn compare_run_rejects_zero_exit_when_the_common_verifier_fails() {
    let source = tempdir().expect("temporary source repository");
    let runs = tempdir().expect("temporary run parent");
    initialize_compare_source(source.path());
    let run = runs.path().join("compare-verifier-failure");
    let prepare = run_cli(
        source.path(),
        &[
            "coder",
            "compare",
            "prepare",
            run.to_str().expect("UTF-8 run path"),
            "--source",
            source.path().to_str().expect("UTF-8 source path"),
            "--commit",
            "main",
        ],
    );
    assert!(prepare.status.success());
    let task = runs.path().join("task.txt");
    fs::write(&task, "task").expect("task file");
    let fake = write_fake_harness(runs.path());

    let output = std_command(env!("CARGO_BIN_EXE_liberado"))
        .args([
            "coder",
            "compare",
            "run",
            run.to_str().expect("UTF-8 run path"),
            "--task",
            task.to_str().expect("UTF-8 task path"),
            "--api-key-env",
            "LIBERADO_COMPARE_TEST_KEY",
            "--liberado-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
            "--pi-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
        ])
        .env("LIBERADO_COMPARE_TEST_KEY", "test-only")
        .env("LIBERADO_COMPARE_BREAK_BUILD", "1")
        .current_dir(source.path())
        .output()
        .expect("comparison run starts");
    assert!(
        !output.status.success(),
        "a red common verifier must fail the comparison"
    );

    for harness in ["liberado", "pi"] {
        let artifacts = run.join("artifacts").join(harness);
        let result: serde_json::Value =
            serde_json::from_slice(&fs::read(artifacts.join("result.json")).expect("saved result"))
                .expect("valid saved result");
        assert_eq!(result["exit_code"], 0);
        assert_ne!(result["verifier_exit_code"], 0);
        assert_eq!(
            git_capture_test(&run.join("worktrees").join(harness), &["status", "--short"]),
            ""
        );
    }

    remove_compare_worktrees(source.path(), &run);
}

#[test]
fn compare_run_applies_hidden_acceptance_overlay_to_both_harnesses() {
    let source = tempdir().expect("temporary source repository");
    let runs = tempdir().expect("temporary run parent");
    initialize_compare_source(source.path());
    let run = runs.path().join("compare-hidden-acceptance");
    let prepare = run_cli(
        source.path(),
        &[
            "coder",
            "compare",
            "prepare",
            run.to_str().expect("UTF-8 run path"),
            "--source",
            source.path().to_str().expect("UTF-8 source path"),
            "--commit",
            "main",
        ],
    );
    assert!(prepare.status.success());
    let task = runs.path().join("task.txt");
    fs::write(&task, "task").expect("task file");
    let overlay = runs.path().join("hidden-overlay");
    fs::create_dir_all(overlay.join("tests")).expect("overlay tests directory");
    fs::write(
        overlay.join("tests").join("hidden_acceptance.rs"),
        r#"#[test]
fn result_matches_hidden_contract() {
    assert_eq!(include_str!("../tracked.txt"), "accepted result\n");
}
"#,
    )
    .expect("hidden acceptance test");
    let fake = write_fake_harness(runs.path());

    let output = std_command(env!("CARGO_BIN_EXE_liberado"))
        .args([
            "coder",
            "compare",
            "run",
            run.to_str().expect("UTF-8 run path"),
            "--task",
            task.to_str().expect("UTF-8 task path"),
            "--acceptance-overlay",
            overlay.to_str().expect("UTF-8 overlay path"),
            "--api-key-env",
            "LIBERADO_COMPARE_TEST_KEY",
            "--liberado-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
            "--pi-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
        ])
        .env("LIBERADO_COMPARE_TEST_KEY", "test-only")
        .current_dir(source.path())
        .output()
        .expect("comparison run starts");
    assert!(
        !output.status.success(),
        "hidden contract must reject both results"
    );

    for harness in ["liberado", "pi"] {
        let worktree = run.join("worktrees").join(harness);
        let result: serde_json::Value = serde_json::from_slice(
            &fs::read(run.join("artifacts").join(harness).join("result.json"))
                .expect("saved result"),
        )
        .expect("valid saved result");
        assert_ne!(result["verifier_exit_code"], 0);
        assert!(
            !worktree.join("tests").join("hidden_acceptance.rs").exists(),
            "hidden oracle must be removed before result preservation"
        );
        assert_eq!(git_capture_test(&worktree, &["status", "--short"]), "");
    }
    assert!(
        run.join("acceptance-overlay")
            .join("tests")
            .join("hidden_acceptance.rs")
            .is_file(),
        "captured oracle must remain with the run"
    );
    let pins = fs::read_to_string(run.join("pins.txt")).expect("captured pins");
    assert!(!pins.contains("acceptance_overlay_hash=none"), "{pins}");

    remove_compare_worktrees(source.path(), &run);
}

#[test]
fn compare_run_refuses_an_acceptance_overlay_that_would_overwrite_source() {
    let source = tempdir().expect("temporary source repository");
    let runs = tempdir().expect("temporary run parent");
    initialize_compare_source(source.path());
    let run = runs.path().join("compare-overwriting-acceptance");
    let prepare = run_cli(
        source.path(),
        &[
            "coder",
            "compare",
            "prepare",
            run.to_str().expect("UTF-8 run path"),
            "--source",
            source.path().to_str().expect("UTF-8 source path"),
            "--commit",
            "main",
        ],
    );
    assert!(prepare.status.success());
    let task = runs.path().join("task.txt");
    fs::write(&task, "task").expect("task file");
    let overlay = runs.path().join("overwriting-overlay");
    fs::create_dir_all(overlay.join("src")).expect("overlay source directory");
    fs::write(overlay.join("src").join("lib.rs"), "not allowed\n")
        .expect("overwriting acceptance file");
    let fake = write_fake_harness(runs.path());

    let output = std_command(env!("CARGO_BIN_EXE_liberado"))
        .args([
            "coder",
            "compare",
            "run",
            run.to_str().expect("UTF-8 run path"),
            "--task",
            task.to_str().expect("UTF-8 task path"),
            "--acceptance-overlay",
            overlay.to_str().expect("UTF-8 overlay path"),
            "--api-key-env",
            "LIBERADO_COMPARE_TEST_KEY",
            "--liberado-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
            "--pi-bin",
            fake.to_str().expect("UTF-8 fake harness path"),
        ])
        .env("LIBERADO_COMPARE_TEST_KEY", "test-only")
        .current_dir(source.path())
        .output()
        .expect("comparison run starts");
    assert!(!output.status.success());

    for harness in ["liberado", "pi"] {
        let worktree = run.join("worktrees").join(harness);
        let result: serde_json::Value = serde_json::from_slice(
            &fs::read(run.join("artifacts").join(harness).join("result.json"))
                .expect("saved result"),
        )
        .expect("valid saved result");
        assert_eq!(result["verifier_exit_code"], 125);
        assert_eq!(
            fs::read_to_string(worktree.join("src").join("lib.rs"))
                .expect("source file")
                .replace("\r\n", "\n"),
            "pub fn fixture() {}\n"
        );
        assert_eq!(git_capture_test(&worktree, &["status", "--short"]), "");
    }

    remove_compare_worktrees(source.path(), &run);
}

fn initialize_compare_source(root: &Path) {
    git_test(root, &["init", "-b", "main"]);
    git_test(root, &["config", "user.email", "test@example.com"]);
    git_test(root, &["config", "user.name", "Test"]);
    fs::write(root.join("tracked.txt"), "base\n").expect("tracked source file");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"compare-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("workspace manifest");
    fs::write(
        root.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"compare-fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("workspace lockfile");
    fs::create_dir(root.join("src")).expect("source directory");
    fs::write(root.join("src").join("lib.rs"), "pub fn fixture() {}\n").expect("fixture source");
    fs::write(
        root.join(".gitignore"),
        "turbovault/\nturbomcp/\ncoder-traces/\ntarget/\n",
    )
    .expect("gitignore");
    for sibling in ["turbovault", "turbomcp"] {
        let path = root.join(sibling);
        fs::create_dir(&path).expect("sibling checkout");
        fs::write(path.join("copied.txt"), sibling).expect("sibling marker");
    }
    git_test(
        root,
        &[
            "add",
            "tracked.txt",
            ".gitignore",
            "Cargo.toml",
            "Cargo.lock",
            "src/lib.rs",
        ],
    );
    git_test(root, &["commit", "-m", "base"]);
}

#[cfg(windows)]
fn write_fake_harness(root: &Path) -> std::path::PathBuf {
    let path = root.join("fake-harness.cmd");
    fs::write(
        &path,
        "@echo off\r\necho fake result>tracked.txt\r\nif \"%LIBERADO_COMPARE_BREAK_BUILD%\"==\"1\" echo this is not Rust>src\\lib.rs\r\nif not exist coder-traces mkdir coder-traces\r\necho {}>coder-traces\\compare-run-liberado.fake.json\r\necho fake stdout\r\necho fake stderr 1>&2\r\nexit /b 0\r\n",
    )
    .expect("fake Windows harness");
    path
}

#[cfg(unix)]
fn write_fake_harness(root: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = root.join("fake-harness.sh");
    fs::write(
        &path,
        "#!/bin/sh\nprintf 'fake result\\n' > tracked.txt\nif [ \"$LIBERADO_COMPARE_BREAK_BUILD\" = \"1\" ]; then printf 'this is not Rust\\n' > src/lib.rs; fi\nmkdir -p coder-traces\nprintf '{}\\n' > coder-traces/compare-run-liberado.fake.json\necho fake stdout\necho fake stderr >&2\nexit 0\n",
    )
    .expect("fake Unix harness");
    let mut permissions = fs::metadata(&path)
        .expect("fake harness metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("executable fake harness");
    path
}

fn remove_compare_worktrees(source: &Path, run: &Path) {
    for harness in ["liberado", "pi"] {
        let path = run.join("worktrees").join(harness);
        if path.exists() {
            let status = std_command("git")
                .arg("-C")
                .arg(source)
                .args(["worktree", "remove", "--force"])
                .arg(&path)
                .status()
                .expect("git worktree remove starts");
            assert!(status.success(), "remove {harness} worktree");
        }
    }
}

fn git_capture_test(root: &Path, args: &[&str]) -> String {
    let output = std_command("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git starts");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn compare_reset_restores_tracked_files_and_preserves_untracked_files() {
    let temp = tempdir().expect("temporary workspace");
    let workspace = temp.path().join("compare-workspace");
    fs::create_dir(&workspace).expect("workspace directory");
    git_test(&workspace, &["init"]);
    fs::write(workspace.join("tracked.txt"), "base\n").expect("tracked file");
    git_test(&workspace, &["config", "user.email", "test@example.com"]);
    git_test(&workspace, &["config", "user.name", "Test"]);
    git_test(&workspace, &["add", "."]);
    git_test(&workspace, &["commit", "-m", "base"]);
    fs::write(workspace.join("tracked.txt"), "changed\n").expect("change tracked file");
    fs::write(workspace.join("scratch.txt"), "keep me\n").expect("untracked file");

    let output = run_cli(
        temp.path(),
        &[
            "coder",
            "compare",
            "reset",
            workspace.to_str().expect("UTF-8 workspace path"),
        ],
    );

    assert!(
        output.status.success(),
        "reset failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Git applies the repository's configured checkout line ending policy.
    // The reset must restore the text, whether that policy produces LF or CRLF.
    let restored = fs::read_to_string(workspace.join("tracked.txt")).expect("restored file");
    assert_eq!(restored.replace("\r\n", "\n"), "base\n");
    assert_eq!(
        fs::read_to_string(workspace.join("scratch.txt")).expect("preserved file"),
        "keep me\n"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("untracked path-deps left in place"));
}

fn git_test(workspace: &Path, args: &[&str]) {
    let status = std_command("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .status()
        .expect("git should start");
    assert!(status.success(), "git {args:?} failed with {status}");
}

#[test]
fn coder_summarize_command_dispatches_and_reports_native_trace() {
    let temp = tempdir().expect("temporary directory");
    let trace = temp.path().join("run.json");
    fs::write(
        &trace,
        r#"{
          "request": {"attempt": 2, "config": {"coder": {"max_turns": 4, "model": "test-model", "reasoning": "low"}}},
          "events": [
            {"type": "model_turn_finished", "at": "2026-08-14T12:00:00Z"},
            {"type": "tool_started", "tool": "edit_file", "at": "2026-08-14T12:00:01Z"},
            {"type": "session_finished", "summary": "done", "at": "2026-08-14T12:00:02Z"}
          ]
        }"#,
    )
    .expect("trace JSON");

    let output = run_cli(
        temp.path(),
        &[
            "coder",
            "summarize",
            trace.to_str().expect("UTF-8 trace path"),
        ],
    );

    assert!(
        output.status.success(),
        "summarize failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("## Liberado  run.json"));
    assert!(stdout.contains("attempt: 2"));
    assert!(stdout.contains("turns: 1"));
    assert!(stdout.contains("edit_file: 1"));
    assert!(stdout.contains("session_finished: done"));
}

fn checkout_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory")
        .parent()
        .expect("repository root")
        .to_path_buf()
}

#[test]
fn mutants_requires_a_subcommand() {
    let temp = tempdir().unwrap();
    let stderr = run_usage(temp.path(), &["mutants"]);
    assert!(stderr.contains("usage: liberado mutants"), "{stderr}");
}

#[test]
fn mutants_report_runs_from_the_checkout() {
    let root = checkout_root();
    let output = run_cli(&root, &["mutants", "report"]);
    assert!(
        output.status.success(),
        "mutants report failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Never campaigned"));
    assert!(stdout.contains("Historical only"));
    assert!(stdout.contains("Most drift"));
}

#[test]
fn mutants_next_suggests_a_never_campaigned_crate_first() {
    let root = checkout_root();
    let output = run_cli(&root, &["mutants", "next"]);
    assert!(
        output.status.success(),
        "mutants next failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(!name.is_empty(), "expected a crate directory name");
    assert!(
        root.join("crates").join(&name).is_dir(),
        "{name} is not a crate directory"
    );
}

#[test]
fn mutants_record_ingests_outcomes_json() {
    let temp = tempdir().expect("temporary repository");
    let root = temp.path();
    fs::create_dir_all(root.join("crates/markdown")).expect("crate directory");
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("workspace");
    fs::write(
        root.join("crates/markdown/Cargo.toml"),
        "[package]\nname = \"liberado-markdown\"\n\n[package.metadata.liberado]\nrole = \"client\"\n",
    )
    .expect("manifest");
    fs::write(
        root.join("mutants-ledger.json"),
        "{\"schema\":1,\"campaigns\":[]}\n",
    )
    .expect("ledger");
    fs::create_dir_all(root.join("mutants.out")).expect("mutants output");
    fs::write(
        root.join("mutants.out/outcomes.json"),
        // total_mutants must be present and equal the bucket sum: an
        // undeclared total cannot prove the run finished, and the recorder
        // refuses it.
        r#"{
  "outcomes": [{"scenario": {"Mutant": {"package": "liberado-markdown"}}}],
  "total_mutants": 3,
  "caught": 2,
  "missed": 1,
  "timeout": 0,
  "unviable": 0,
  "cargo_mutants_version": "27.1.0"
}"#,
    )
    .expect("outcomes");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(root)
        .status()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(root)
        .status()
        .expect("git identity");
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(root)
        .status()
        .expect("git identity");
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .status()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "seed"])
        .current_dir(root)
        .status()
        .expect("git commit");

    let output = run_cli(root, &["mutants", "record", "markdown"]);
    assert!(
        output.status.success(),
        "mutants record failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ledger: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join("mutants-ledger.json")).expect("ledger file"),
    )
    .expect("ledger json");
    let campaigns = ledger["campaigns"].as_array().expect("campaigns");
    assert_eq!(campaigns.len(), 1);
    assert_eq!(campaigns[0]["package"], "liberado-markdown");
    assert_eq!(campaigns[0]["counts"]["caught"], 2);
    assert_eq!(campaigns[0]["counts"]["survived"], 1);
    assert!(campaigns[0]["commit"].as_str().is_some());
}

/// `docs crate-map --write` must regenerate the map file. A missing map makes the check arm
/// fail, so a created file is proof the `--write` guard actually matched — the surviving
/// mutant rewrote the guard to `false`, turning every write into a doomed check.
#[test]
fn docs_crate_map_write_flag_generates_the_map() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("crates")).expect("crates directory");
    fs::create_dir_all(root.join("docs/spec/reference")).expect("map directory");
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("workspace manifest");

    let output = run_cli(root, &["docs", "crate-map", "--write"]);
    assert!(
        output.status.success(),
        "crate-map --write failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        root.join("docs/spec/reference/crate-map.md").is_file(),
        "--write must generate the crate map file"
    );
}

/// `mutants run <dir>` reaches the crate resolver: an unknown crate name fails with the
/// resolver's error, not the argument-usage text a dropped dispatch arm would print.
#[test]
fn mutants_run_reaches_the_crate_resolver() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("crates")).expect("crates directory");
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").expect("workspace manifest");

    let stderr = run_usage(root, &["mutants", "run", "not-a-crate"]);
    assert!(
        stderr.contains("unknown crate directory"),
        "expected the resolver error, got: {stderr}"
    );
    assert!(
        !stderr.contains("usage: liberado mutants run"),
        "the run arm must dispatch, not fall through to usage: {stderr}"
    );
}

/// `serve` with an unloadable config is a hard error (Decision 14 fail-fast): the daemon
/// never starts and the process exits non-zero. A `run_serve` body replaced by `Ok(())`
/// would exit 0 without reading any config.
#[test]
fn serve_with_an_unloadable_config_fails() {
    let temp = tempdir().unwrap();
    let config_dir = temp.path().join("config");
    fs::create_dir(&config_dir).expect("config directory");
    fs::write(config_dir.join("topology.toml"), "not=[valid toml").expect("garbage topology");

    let output = std_command(env!("CARGO_BIN_EXE_liberado"))
        .env("LIBERADO_CONFIG_DIR", &config_dir)
        .args(["serve", "/nonexistent-vault-for-mutant-test"])
        .current_dir(temp.path())
        .output()
        .expect("liberado CLI should start");

    assert!(
        !output.status.success(),
        "serve with a garbage config must fail, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
