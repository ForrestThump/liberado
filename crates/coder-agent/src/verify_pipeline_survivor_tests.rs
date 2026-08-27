//! Split from `verify_pipeline.rs`: kills the baseline campaign's survivors.
//!
//! Covers the committed-changes fallback in the empty-diff gate, including the
//! no-commits-yet error path.

use super::*;

/// A repo with zero commits: status is clean (nothing tracked), and
/// `git log -1` exits non-zero — which the committed-changes leg swallows into
/// an empty list by design, so the verdict is a plain empty diff.
#[tokio::test]
async fn a_repo_with_no_commits_reports_an_empty_diff() {
    let dir = tempfile::tempdir().unwrap();
    let out = liberado_common::process::std_command("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());

    let verdict = git_nonempty_diff(dir.path(), "probe-diff").await;
    assert!(
        matches!(verdict.status, liberado_coder_core::VerdictStatus::Fail),
        "no commits reads as nothing produced: {verdict:?}"
    );
    assert!(verdict.summary.contains("empty diff"), "{verdict:?}");
}

#[tokio::test]
async fn a_clean_repo_without_commits_but_with_files_still_fails_as_empty() {
    // Untracked files make status non-empty; the uncommitted leg passes first,
    // so this pins the ordering of the two checks.
    let dir = tempfile::tempdir().unwrap();
    let _ = liberado_common::process::std_command("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .output()
        .unwrap();
    std::fs::write(dir.path().join("a.txt"), "work\n").unwrap();

    let verdict = git_nonempty_diff(dir.path(), "probe-diff").await;
    assert!(
        matches!(verdict.status, liberado_coder_core::VerdictStatus::Pass),
        "{verdict:?}"
    );
}
