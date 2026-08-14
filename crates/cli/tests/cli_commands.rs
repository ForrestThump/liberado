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
fn compare_prepare_is_print_only_and_uses_the_current_repository() {
    let temp = tempdir().expect("temporary repository");
    fs::create_dir(temp.path().join("crates")).expect("crates directory");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .expect("Cargo.toml");

    let output = run_cli(temp.path(), &["coder", "compare", "prepare"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("print only. No harness started."));
    assert!(stdout.contains("Provider: openrouter"));
    assert!(stdout.contains("--mode coding"));
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
