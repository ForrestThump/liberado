//! Campaign tests that killed catchable coder-tools survivors after the
//! 81-to-35 re-run. Split from `tool_survivor_tests.rs` so the original
//! sibling stays under the module-health function review boundary.
//!
//! Wired as a sibling of `git.rs`. Reuses the identity-free repo fixtures
//! from `tool_survivor_tests`.

use super::tool_survivor_tests::{
    repo_without_user_config_for_tests, run_git_for_tests, tool_runtime,
};
use crate::CodingToolRuntime;
use liberado_coder_core::{CommandPolicy, HashlineConfig, PathPolicy};
use liberado_executor::ToolRuntime;
use liberado_provider::ToolInvocation;
use serde_json::json;

fn rev_parse(dir: &std::path::Path, rev: &str) -> String {
    let out = liberado_common::process::std_command("git")
        .args(["rev-parse", rev])
        .current_dir(dir)
        .output()
        .expect("rev-parse runs");
    assert!(out.status.success(), "rev-parse {rev} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ── crate-root helpers ──────────────────────────────────────────────────────

#[test]
fn coding_worktrees_base_always_ends_with_the_coding_worktrees_dir() {
    let p = crate::coding_worktrees_base();
    assert_eq!(
        p.file_name().and_then(|n| n.to_str()),
        Some("coding-worktrees"),
        "the durable worktree root is a named directory, not an empty PathBuf: {p:?}"
    );
}

#[test]
fn durable_session_workspace_keeps_a_safe_id_under_the_base() {
    let p = crate::durable_session_workspace("sess-1").expect("a safe id is Some");
    assert_eq!(
        p.file_name().and_then(|n| n.to_str()),
        Some("sess-1"),
        "{p:?}"
    );
    assert!(
        p.components().any(|c| c.as_os_str() == "coding-worktrees"),
        "must sit under coding-worktrees, not an empty default: {p:?}"
    );
    assert!(
        crate::durable_session_workspace("").is_none(),
        "an empty id is not a directory name"
    );
    assert!(
        crate::durable_session_workspace("a/b").is_none(),
        "a slash is not a safe directory name"
    );
}

#[test]
fn preflight_gh_pr_create_names_the_missing_base_branch() {
    let branch = "this-branch-does-not-exist-on-origin-zzzz";
    let err = crate::preflight_gh_pr_create(
        "gh",
        &["pr".into(), "create".into(), "--base".into(), branch.into()],
    )
    .expect("a missing origin branch is a refusal");
    assert!(
        err.contains(branch),
        "the error must name the --base value, not the previous argv token: {err}"
    );
}

#[test]
fn preflight_gh_pr_create_requires_both_pr_and_create() {
    // `gh pr --base X` is not `gh pr create`. `&&` vs `||` is the difference.
    assert!(
        crate::preflight_gh_pr_create(
            "gh",
            &[
                "pr".into(),
                "--base".into(),
                "this-branch-does-not-exist-on-origin-zzzz".into(),
            ],
        )
        .is_none(),
        "without create this is not a PR-create preflight"
    );
    assert!(
        crate::preflight_gh_pr_create(
            "gh",
            &[
                "create".into(),
                "--base".into(),
                "this-branch-does-not-exist-on-origin-zzzz".into(),
            ],
        )
        .is_none(),
        "without pr this is not a PR-create preflight"
    );
}

#[test]
fn command_grants_share_the_workspace_set_not_a_fresh_default() {
    let (_dir, runtime) = tool_runtime();
    runtime.command_grants().allow("rg");
    assert!(
        runtime.command_grants().contains("rg"),
        "allow() must stick on the runtime's grant set, not a throwaway Default"
    );
}

#[test]
fn hashline_getter_returns_the_configured_value_not_a_fresh_default() {
    let (_dir, runtime) = tool_runtime();
    let runtime = runtime.with_hashline(HashlineConfig {
        enabled: true,
        hash_length: 10,
    });
    assert!(runtime.hashline().enabled);
    assert_eq!(runtime.hashline().hash_length, 10);
}

#[test]
fn default_diff_mode_is_patch() {
    assert_eq!(crate::default_diff_mode(), "patch");
}

#[test]
fn default_remote_is_origin() {
    assert_eq!(crate::default_remote(), "origin");
}

#[test]
fn default_output_mode_is_files_with_matches() {
    assert_eq!(crate::default_output_mode(), "files_with_matches");
}

#[test]
fn identifiers_keep_tokens_of_four_or_more() {
    let got = crate::identifiers("fn foo bar_x ab");
    assert!(
        got.iter().any(|t| t == "bar_x"),
        "a 5-char token is kept: {got:?}"
    );
    assert!(
        !got.iter().any(|t| t == "fn" || t == "foo" || t == "ab"),
        "tokens shorter than 4 are noise: {got:?}"
    );
}

#[test]
fn glob_match_resumes_after_the_star_not_at_it() {
    assert!(crate::glob_match("ab*c", "abXYZc"));
    assert!(!crate::glob_match("ab*c", "abXYZ"));
    assert!(crate::glob_match("a*c", "ac"), "empty star match");
}

#[test]
fn is_comment_line_accepts_each_c_family_form() {
    assert!(crate::is_comment_line("// fn hidden() {}"));
    assert!(crate::is_comment_line("/* fn hidden() {} */"));
    assert!(crate::is_comment_line("* continuation"));
    assert!(!crate::is_comment_line("fn real() {}"));
}

#[test]
fn extract_impl_symbol_skips_trait_impl_for_and_empty_names() {
    assert_eq!(
        crate::extract_impl_symbol("impl Foo {"),
        Some("impl Foo".into())
    );
    assert!(
        crate::extract_impl_symbol("impl for Bar {").is_none(),
        "`impl for T` is not a type name"
    );
    assert!(
        crate::extract_impl_symbol("impl {").is_none(),
        "an empty name is not an impl symbol"
    );
}

#[test]
fn ts_class_does_not_treat_extends_as_a_class_name() {
    assert_eq!(crate::ts_class("class Foo {"), Some("class Foo".into()));
    assert!(
        crate::ts_class("class extends Bar {").is_none(),
        "the token after class must not be a heritage keyword"
    );
    assert!(crate::ts_class("class implements I {").is_none());
}

#[test]
fn read_only_tools_are_not_every_tool() {
    let (_dir, runtime) = tool_runtime();
    assert!(runtime.is_read_only("read_file"));
    assert!(runtime.is_read_only("git_diff"));
    assert!(
        !runtime.is_read_only("write_file"),
        "a write tool is not concurrent-safe"
    );
    assert!(
        !runtime.is_read_only("edit_file"),
        "an edit tool is not concurrent-safe"
    );
}

// ── grep ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn grep_glob_is_an_allow_filter_not_a_deny_filter() {
    let (_dir, runtime) = tool_runtime();
    std::fs::write(_dir.path().join("keep.rs"), "needle\n").unwrap();
    std::fs::write(_dir.path().join("skip.txt"), "needle\n").unwrap();
    let out = runtime
        .invoke_json("grep", json!({"pattern": "needle", "glob": "*.rs"}))
        .await
        .expect("grep runs");
    let rendered = out.to_string();
    assert!(rendered.contains("keep.rs"), "{rendered}");
    assert!(
        !rendered.contains("skip.txt"),
        "glob is allow, not invert: {rendered}"
    );
}

#[tokio::test]
async fn grep_content_line_numbers_are_one_based() {
    let (_dir, runtime) = tool_runtime();
    std::fs::write(_dir.path().join("hay.rs"), "aaa\nneedle\nccc\n").unwrap();
    let out = runtime
        .invoke_json(
            "grep",
            json!({"pattern": "needle", "output_mode": "content"}),
        )
        .await
        .expect("grep runs");
    assert_eq!(
        out["matches"][0]["line"], 2,
        "the second line is 2, not idx*1: {out}"
    );
}

#[tokio::test]
async fn grep_files_with_matches_omits_files_with_zero_hits() {
    let (_dir, runtime) = tool_runtime();
    std::fs::write(_dir.path().join("hit.rs"), "needle\n").unwrap();
    std::fs::write(_dir.path().join("miss.rs"), "nothing\n").unwrap();
    let out = runtime
        .invoke_json("grep", json!({"pattern": "needle"}))
        .await
        .expect("grep runs");
    let files = out["files"].as_array().cloned().unwrap_or_default();
    let rendered = format!("{files:?}");
    assert!(rendered.contains("hit.rs"), "{out}");
    assert!(
        !rendered.contains("miss.rs"),
        "zero-hit files are not matches: {out}"
    );
}

#[tokio::test]
async fn grep_head_limit_is_a_strict_upper_bound() {
    let (_dir, runtime) = tool_runtime();
    std::fs::write(_dir.path().join("a.rs"), "needle\n").unwrap();
    std::fs::write(_dir.path().join("b.rs"), "needle\n").unwrap();
    let out = runtime
        .invoke_json("grep", json!({"pattern": "needle", "head_limit": 1}))
        .await
        .expect("grep runs");
    let files = out["files"].as_array().expect("files mode");
    assert_eq!(
        files.len(),
        1,
        "head_limit 1 keeps one file, not two: {out}"
    );
}

#[tokio::test]
async fn grep_count_mode_also_respects_head_limit() {
    let (_dir, runtime) = tool_runtime();
    std::fs::write(_dir.path().join("a.rs"), "needle\n").unwrap();
    std::fs::write(_dir.path().join("b.rs"), "needle\n").unwrap();
    let out = runtime
        .invoke_json(
            "grep",
            json!({"pattern": "needle", "output_mode": "count", "head_limit": 1}),
        )
        .await
        .expect("grep runs");
    let counts = out["counts"].as_array().expect("count mode");
    assert_eq!(counts.len(), 1, "{out}");
}

#[tokio::test]
async fn grep_did_you_mean_dedups_identical_line_text() {
    let (_dir, runtime) = tool_runtime();
    std::fs::write(_dir.path().join("hay.rs"), "observer\nobserver\n").unwrap();
    let out = runtime
        .invoke_json("grep", json!({"pattern": "observers"}))
        .await
        .expect("no exact match still returns");
    let suggestions = out["did_you_mean"].as_array().cloned().unwrap_or_default();
    let texts: Vec<_> = suggestions
        .iter()
        .filter_map(|s| s["text"].as_str())
        .collect();
    let unique = texts.len()
        == texts
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len();
    assert!(unique, "identical near-miss lines collapse: {out}");
}

// ── read / write / edit ─────────────────────────────────────────────────────

#[tokio::test]
async fn invoke_json_for_backend_returns_the_real_tool_payload() {
    let (_dir, runtime) = tool_runtime();
    std::fs::write(_dir.path().join("hay.rs"), "needle\n").unwrap();
    let out = runtime
        .invoke_json_for_backend("grep", json!({"pattern": "needle"}))
        .await
        .expect("backend invoke is not a stub");
    assert!(
        out.is_object(),
        "Default::default() for Value is null: {out}"
    );
    assert!(
        out.get("files").is_some() || out.get("total").is_some(),
        "{out}"
    );
}

#[tokio::test]
async fn hashline_read_does_not_prefix_the_first_line_with_a_newline() {
    let (_dir, runtime) = tool_runtime();
    let runtime = runtime.with_hashline(HashlineConfig {
        enabled: true,
        hash_length: 7,
    });
    std::fs::write(_dir.path().join("f.rs"), "alpha\nbeta\n").unwrap();
    let out = runtime
        .invoke_json("read_file", json!({"path": "f.rs"}))
        .await
        .expect("read runs");
    let content = out["content"].as_str().expect("content");
    // Numbered body follows the header; the first numbered line is `1:alpha`, not `\n1:alpha`.
    assert!(
        content.contains("1:alpha\n2:beta"),
        "newlines separate numbered lines, and only after the first: {content:?}"
    );
    assert!(
        !content.contains("\n\n1:alpha"),
        "i > 0 must not fire on the first numbered line: {content:?}"
    );
}

#[tokio::test]
async fn write_file_allows_a_fifty_line_file_to_be_replaced_by_one_line() {
    let (_dir, runtime) = tool_runtime();
    let body = (0..50)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(_dir.path().join("f.rs"), body).unwrap();
    runtime
        .invoke_json(
            "write_file",
            json!({"path": "f.rs", "content": "only\n", "overwrite": true}),
        )
        .await
        .expect("exactly 50 lines is not over the truncation floor");
}

#[tokio::test]
async fn write_file_allows_a_rewrite_that_is_exactly_one_fifth() {
    let (_dir, runtime) = tool_runtime();
    let body = (0..100)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(_dir.path().join("f.rs"), body).unwrap();
    let new_body = (0..20)
        .map(|i| format!("n{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    runtime
        .invoke_json(
            "write_file",
            json!({"path": "f.rs", "content": new_body, "overwrite": true}),
        )
        .await
        .expect("after * 5 == before is not a truncation");
}

#[tokio::test]
async fn write_file_refuses_a_real_truncation() {
    let (_dir, runtime) = tool_runtime();
    let body = (0..100)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(_dir.path().join("f.rs"), body).unwrap();
    let err = runtime
        .invoke_json(
            "write_file",
            json!({"path": "f.rs", "content": "tiny\n", "overwrite": true}),
        )
        .await
        .expect_err("100 lines to 1 is a truncation");
    assert!(err.to_string().contains("truncation"), "{err}");
}

// ── git_diff / untracked ────────────────────────────────────────────────────

#[tokio::test]
async fn git_diff_name_only_lists_paths_not_hunks() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);
    std::fs::write(repo.join("tracked.txt"), "two\n").unwrap();

    let runtime = CodingToolRuntime::new(
        repo.as_path(),
        CommandPolicy::default(),
        PathPolicy::default(),
    )
    .unwrap();
    let out = runtime
        .invoke_json("git_diff", json!({"mode": "name_only"}))
        .await
        .expect("name_only is a real mode");
    let stdout = out["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.contains("tracked.txt"),
        "name_only lists the path: {stdout}"
    );
    assert!(
        !stdout.contains("@@"),
        "name_only must not fall through to patch: {stdout}"
    );
}

#[tokio::test]
async fn git_diff_default_mode_is_patch() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);
    std::fs::write(repo.join("tracked.txt"), "two\n").unwrap();

    let runtime = CodingToolRuntime::new(
        repo.as_path(),
        CommandPolicy::default(),
        PathPolicy::default(),
    )
    .unwrap();
    let out = runtime
        .invoke_json("git_diff", json!({}))
        .await
        .expect("omitted mode defaults to patch");
    assert_eq!(out["mode"], "patch", "{out}");
}

#[tokio::test]
async fn git_diff_untracked_only_does_not_grow_a_leading_blank_line() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("seed.txt"), "one\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);
    std::fs::write(repo.join("new.txt"), "fresh\n").unwrap();

    let runtime = CodingToolRuntime::new(
        repo.as_path(),
        CommandPolicy::default(),
        PathPolicy::default(),
    )
    .unwrap();
    let out = runtime
        .invoke_json("git_diff", json!({"mode": "name_only"}))
        .await
        .expect("untracked files still show");
    let stdout = out["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.starts_with("# untracked"),
        "an empty tracked diff is replaced by the untracked section, not prepended with a blank: {stdout:?}"
    );
    assert!(stdout.contains("new.txt"), "{stdout}");
}

#[tokio::test]
async fn untracked_name_only_does_not_inline_file_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("seed.txt"), "one\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);
    std::fs::write(repo.join("new.txt"), "secret-body\n").unwrap();

    let runtime = CodingToolRuntime::new(
        repo.as_path(),
        CommandPolicy::default(),
        PathPolicy::default(),
    )
    .unwrap();
    let out = runtime
        .invoke_json("git_diff", json!({"mode": "name_only"}))
        .await
        .unwrap();
    let stdout = out["stdout"].as_str().unwrap_or_default();
    assert!(stdout.contains("new.txt"), "{stdout}");
    assert!(
        !stdout.contains("secret-body"),
        "name_only must not fall through to the patch arm: {stdout}"
    );
    assert!(
        !stdout.contains("+secret"),
        "name_only must not prefix bodies: {stdout}"
    );
}

#[tokio::test]
async fn untracked_stat_reports_line_counts_not_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("seed.txt"), "one\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);
    std::fs::write(repo.join("new.txt"), "a\nb\nc\n").unwrap();

    let runtime = CodingToolRuntime::new(
        repo.as_path(),
        CommandPolicy::default(),
        PathPolicy::default(),
    )
    .unwrap();
    let out = runtime
        .invoke_json("git_diff", json!({"mode": "stat"}))
        .await
        .unwrap();
    let stdout = out["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.contains("new.txt") && stdout.contains('|') && stdout.contains("+++"),
        "stat names the file and its added-line count: {stdout}"
    );
    assert!(
        !stdout.contains("--- new file"),
        "stat must not fall through to the patch arm: {stdout}"
    );
}

#[tokio::test]
async fn untracked_patch_marks_truncation_only_when_the_body_was_cut() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("seed.txt"), "one\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);
    std::fs::write(repo.join("small.txt"), "short\n").unwrap();

    let runtime = CodingToolRuntime::new(
        repo.as_path(),
        CommandPolicy::default(),
        PathPolicy::default(),
    )
    .unwrap();
    let out = runtime
        .invoke_json("git_diff", json!({"mode": "patch"}))
        .await
        .unwrap();
    let stdout = out["stdout"].as_str().unwrap_or_default();
    assert!(stdout.contains("+short"), "{stdout}");
    assert!(
        !stdout.contains("truncated"),
        "a body under the budget is not truncated: {stdout}"
    );
}

#[tokio::test]
async fn untracked_patch_says_truncated_when_the_body_exceeds_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("seed.txt"), "one\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);
    // UNTRACKED_PATCH_BUDGET is 24_000 bytes.
    let big = "x".repeat(30_000);
    std::fs::write(repo.join("big.txt"), &big).unwrap();

    let runtime = CodingToolRuntime::new(
        repo.as_path(),
        CommandPolicy::default(),
        PathPolicy::default(),
    )
    .unwrap();
    let out = runtime
        .invoke_json("git_diff", json!({"mode": "patch"}))
        .await
        .unwrap();
    let stdout = out["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.contains("truncated"),
        "a body over the budget must say so: {stdout}"
    );
}

// ── explore invoke ──────────────────────────────────────────────────────────

#[tokio::test]
async fn explore_policy_refuses_write_tools_and_allows_reads() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.rs"), "hi\n").unwrap();
    let runtime = CodingToolRuntime::new(
        dir.path(),
        CommandPolicy::default(),
        PathPolicy::read_only(),
    )
    .unwrap();
    let write = ToolInvocation::new("1", "write_file", json!({"path": "f.rs", "content": "x"}));
    let err = runtime.invoke(&write).await.expect_err("writes are closed");
    assert!(
        err.contains("explore") || err.contains("not available"),
        "{err}"
    );
    let read = ToolInvocation::new("2", "read_file", json!({"path": "f.rs"}));
    runtime
        .invoke(&read)
        .await
        .expect("read_file stays in the explore set");
}

// ── git push / fetch actually run ───────────────────────────────────────────

#[test]
fn push_to_an_unknown_remote_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);
    let err = super::push(repo.as_path(), "no-such-remote", None, false)
        .expect_err("a missing remote is not a quiet success");
    assert!(
        !err.message.is_empty(),
        "the error names the failure, not an empty Ok"
    );
}

#[test]
fn fetch_from_an_unknown_remote_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_without_user_config_for_tests(&dir);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    run_git_for_tests(repo.as_path(), &["add", "-A"]);
    run_git_for_tests(repo.as_path(), &["commit", "-qm", "seed"]);
    let err = super::fetch(repo.as_path(), "no-such-remote", None)
        .expect_err("a missing remote is not a quiet success");
    assert!(!err.message.is_empty(), "{err:?}");
}

#[test]
fn fetch_moves_the_remote_tracking_ref() {
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

    std::fs::write(origin.join("b.txt"), "two\n").unwrap();
    run_git_for_tests(origin.as_path(), &["add", "-A"]);
    run_git_for_tests(origin.as_path(), &["commit", "-qm", "origin second"]);
    let origin_head = rev_parse(origin.as_path(), "HEAD");

    for branch in ["master", "main"] {
        if super::fetch(clone.as_path(), "origin", Some(branch)).is_ok() {
            let tracking = format!("refs/remotes/origin/{branch}");
            let fetched = rev_parse(clone.as_path(), &tracking);
            assert_eq!(
                fetched, origin_head,
                "fetch must move the tracking ref, not return Ok empty"
            );
            return;
        }
    }
    panic!("neither master nor main could be fetched from the local origin");
}
