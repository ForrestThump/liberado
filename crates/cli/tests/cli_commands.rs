use liberado_common::process::std_command;
use serde_json::Value;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn run_cli(cwd: &Path, args: &[&str]) -> std::process::Output {
    std_command(env!("CARGO_BIN_EXE_liberado"))
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
        assert!(artifacts.join("git").join("diff.patch").is_file());
        assert!(artifacts.join("result.json").is_file());
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
        "@echo off\r\necho fake result>tracked.txt\r\nif not exist coder-traces mkdir coder-traces\r\necho {}>coder-traces\\compare-run-liberado.fake.json\r\necho fake stdout\r\necho fake stderr 1>&2\r\nexit /b 0\r\n",
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
        "#!/bin/sh\nprintf 'fake result\\n' > tracked.txt\nmkdir -p coder-traces\nprintf '{}\\n' > coder-traces/compare-run-liberado.fake.json\necho fake stdout\necho fake stderr >&2\nexit 0\n",
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
