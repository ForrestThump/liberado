//! Survivor tests for the coder-tools campaign at 2370ca9.
//!
//! Lives beside `git_tests.rs` (already wired from the git module) so the
//! over-boundary lib.rs and hashline.rs gain no lines; crate-root privates
//! are reachable from here as a root descendant via `crate::`.

use crate::CodingToolRuntime;
use liberado_coder_core::{CommandPolicy, PathPolicy};
use liberado_common::process::std_command;
use serde_json::json;

/// A fresh repo with no dependence on host git identity (CI runners have none).
pub(crate) fn repo_without_user_config_for_tests(parent: &tempfile::TempDir) -> std::path::PathBuf {
    let dir = parent.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    run_git_for_tests(&dir, ["init", "--quiet"].as_slice());
    dir
}

pub(crate) fn run_git_for_tests(dir: &std::path::Path, args: &[&str]) {
    let out = std_command("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn tool_runtime() -> (tempfile::TempDir, CodingToolRuntime) {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .expect("runtime builds");
    (dir, runtime)
}

// ── pure helpers on the crate root ───────────────────────────────────────────

#[test]
fn glob_trailing_star_matches_empty_tail() {
    assert!(crate::glob_match("a*", "a"));
    assert!(crate::glob_match("a*", "abc"));
    assert!(crate::glob_match("*", "anything"));
    assert!(!crate::glob_match("a*", "b"));
}

#[test]
fn star_in_the_middle_still_matches_through_it() {
    assert!(crate::glob_match("*b", "ab"));
    assert!(crate::glob_match("a*c", "abc"));
    assert!(crate::glob_match("?.rs", "x.rs"));
    assert!(!crate::glob_match("?.rs", "xy.rs"));
}

/// Each known extension maps to its language name; an unknown one is empty.
#[test]
fn language_names_are_pinned_per_extension() {
    assert_eq!(crate::lang_from_path("a.rs"), "rust");
    assert_eq!(crate::lang_from_path("b.py"), "python");
    assert_eq!(crate::lang_from_path("c.ts"), "typescript");
    assert_eq!(crate::lang_from_path("d.tsx"), "typescript");
    assert_eq!(crate::lang_from_path("e.js"), "typescript");
    assert_eq!(crate::lang_from_path("f.jsx"), "typescript");
    assert_eq!(crate::lang_from_path("g.go"), "go");
    assert_eq!(crate::lang_from_path("h.java"), "java");
    assert_eq!(crate::lang_from_path("i.c"), "");
}

#[test]
fn grep_defaults_list_files_not_a_mystery_mode() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (_dir, runtime) = tool_runtime();
        std::fs::write(_dir.path().join("hay.rs"), "needle_here\n").unwrap();

        let out = runtime
            .invoke_json("grep", json!({"pattern": "needle"}))
            .await
            .expect("the default output mode is files_with_matches");
        let rendered = out.to_string();
        assert!(rendered.contains("hay.rs"), "{rendered}");
    });
}

#[test]
fn grep_default_head_limit_admits_more_than_one_result() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (_dir, runtime) = tool_runtime();
        std::fs::write(_dir.path().join("one.rs"), "needle\n").unwrap();
        std::fs::write(_dir.path().join("two.rs"), "needle\n").unwrap();

        let out = runtime
            .invoke_json("grep", json!({"pattern": "needle"}))
            .await
            .expect("grep runs");
        let rendered = out.to_string();
        assert!(
            rendered.contains("one.rs") && rendered.contains("two.rs"),
            "{rendered}"
        );
    });
}

// ── edit_file near-miss hint gating ─────────────────────────────────────────

#[tokio::test]
async fn a_low_confidence_near_miss_stays_quiet() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();
    std::fs::write(
        dir.path().join("f.rs"),
        "alpha beta gamma delta\nsecond line\n",
    )
    .unwrap();

    let err = runtime
        .invoke_json(
            "edit_file",
            json!({"path": "f.rs", "old": "zzzz qqqq wwww", "new": "x"}),
        )
        .await
        .expect_err("nothing resembles that anchor");
    let msg = err.to_string();
    assert!(
        !msg.contains("closest text"),
        "a <0.6 near miss must not quote itself: {msg}"
    );
}

#[tokio::test]
async fn a_high_confidence_near_miss_quotes_itself() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
            .unwrap();
    std::fs::write(
        dir.path().join("f.rs"),
        "fn alpha_one() {\n    body();\n}\n\nfn beta() {}\n",
    )
    .unwrap();

    let err = runtime
        .invoke_json(
            "edit_file",
            json!({"path": "f.rs", "old": "fn alpha_two() {", "new": "x"}),
        )
        .await
        .expect_err("the anchor text does not exist verbatim");
    let msg = err.to_string();
    assert!(
        msg.contains("closest text"),
        "a >0.6 near miss must be quoted for the model: {msg}"
    );
}

// ── git_diff stat mode ────────────────────────────────────────────────────────

#[tokio::test]
async fn diff_stat_mode_never_returns_patch_hunks() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("tracked.txt"), "one\ntwo\nthree\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);

    std::fs::write(repo.join("tracked.txt"), "one\nTWO\nthree\nplus\n").unwrap();

    let runtime = CodingToolRuntime::new(
        repo.as_path(),
        CommandPolicy::default(),
        PathPolicy::default(),
    )
    .unwrap();
    let out = runtime
        .invoke_json("git_diff", json!({"mode": "stat"}))
        .await
        .expect("stat mode is supported");
    let stdout = out["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.contains('|'),
        "stat output has the counts column: {stdout}"
    );
    assert!(
        !stdout.contains("@@"),
        "stat mode must not return patch hunks: {stdout}"
    );
}

#[tokio::test]
async fn git_log_with_an_empty_branch_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("s.txt"), "initial\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed subject"]);

    // At the tool boundary an empty branch string means "no branch filter"
    // - the same as omitting it - rather than a --branch='' ref error.
    let runtime = CodingToolRuntime::new(
        repo.as_path(),
        CommandPolicy::default(),
        PathPolicy::default(),
    )
    .unwrap();
    let out = runtime
        .invoke_json("git_log", json!({"branch": ""}))
        .await
        .expect("an empty branch is treated as no filter");
    assert_eq!(out["exit_code"], 0, "{out}");
    assert!(
        out["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("seed subject"),
        "{out}"
    );
}

#[tokio::test]
async fn fetch_pulls_from_a_local_remote() {
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = repo_without_user_config_for_tests(&origin_dir);
    std::fs::write(origin.join("a.txt"), "one\n").unwrap();
    run_git_for_tests(origin.as_path(), &["add", "-A"]);
    run_git_for_tests(origin.as_path(), &["commit", "-qm", "origin seed"]);

    let clone_dir = tempfile::tempdir().unwrap();
    let clone = clone_dir.path().join("clone");
    run_git_for_tests(
        clone_dir.path(),
        &["clone", "--quiet", origin.to_str().unwrap(), "clone"],
    );

    // New work lands on the origin after cloning.
    std::fs::write(origin.join("b.txt"), "two\n").unwrap();
    run_git_for_tests(origin.as_path(), &["add", "-A"]);
    run_git_for_tests(origin.as_path(), &["commit", "-qm", "origin second"]);

    for branch in ["master", "main"] {
        if super::fetch(clone.as_path(), "origin", Some(branch)).is_ok() {
            // The fetch contacted the remote: its commit is now reachable
            // through the remote-tracking ref.
            let resolved = std_command("git")
                .args([
                    "rev-parse",
                    "--verify",
                    &format!("refs/remotes/origin/{branch}"),
                ])
                .current_dir(clone.as_path())
                .output()
                .expect("rev-parse runs");
            assert!(
                resolved.status.success(),
                "remote-tracking {branch} must exist"
            );
            return;
        }
    }
    panic!("neither master nor main could be fetched from the local origin");
}

#[tokio::test]
async fn commit_with_an_explicitly_empty_file_list_stages_everything() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("seed.txt"), "v1\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);

    // Modify a TRACKED file, then commit with an explicitly empty list:
    // the contract is stage-everything, so the modification must land.
    std::fs::write(repo.join("seed.txt"), "v2\n").unwrap();
    super::commit(repo.as_path(), "empty list stages all", Some(&[]))
        .expect("an empty file list means stage-all, not stage-nothing");

    let head = super::log(repo.as_path(), 1, None, None).unwrap_or_default();
    assert!(head.contains("empty list stages all"), "{head}");
}

#[tokio::test]
async fn status_reports_a_staged_modification_with_the_porcelain_code() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("t.txt"), "v1\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);

    std::fs::write(repo.join("t.txt"), "v2\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "t.txt"]);

    let status = super::status(repo.as_path()).expect("status reads the staged tree change");
    assert!(
        status
            .lines()
            .any(|l| l.starts_with("M ") && l.contains("t.txt")),
        "staged modifications carry the tree-index M code: {status:?}"
    );
}

// ── hashline parse_patch: leading blanks are skipped, not fatal ─────────────

#[test]
fn leading_blank_lines_do_not_break_a_hashline_patch() {
    use crate::hashline::parse_patch;

    let patch = "\n\n[file.txt ab12cd34]\nCUT 1.=1\n";
    let sections = parse_patch(patch).expect("leading blanks are skipped");
    assert_eq!(sections.len(), 1, "{sections:?}");

    // And a patch that is ONLY blank lines is still the honest empty error.
    let err = parse_patch("\n\n   \n").expect_err("all-blank input has no sections");
    assert!(err.contains("empty"), "{err}");
}
