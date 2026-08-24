//! Split from `fanout.rs`: kills the baseline campaign's survivors.
//!
//! Covers the skip-merge guard for tipless children, the overall verdict
//! conjunction, the child turn budget, LLM conflict resolution end to end, and
//! fence stripping in resolved content.

use super::*;
use liberado_coder_sandbox::{MergeAttempt, add_worktree_on_branch, remove_worktree};
use liberado_provider::CompletionResponse;
use std::path::{Path, PathBuf};

fn git(dir: &Path, args: &[&str]) {
    let ok = liberado_common::process::std_command("git")
        .args(args)
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "git {args:?} failed in {}", dir.display());
}

fn init_repo(dir: &Path) -> PathBuf {
    let root = dir.join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet"]);
    git(&root, &["config", "user.email", "test@liberado.local"]);
    git(&root, &["config", "user.name", "liberado-test"]);
    std::fs::write(root.join("README.md"), "base\n").unwrap();
    git(&root, &["add", "README.md"]);
    git(&root, &["commit", "-m", "base", "--quiet"]);
    root
}

type DynProvider = std::sync::Arc<dyn Provider>;

fn merger_with(
    resolution: Option<&str>,
) -> (DynProvider, std::sync::Arc<liberado_provider::MockProvider>) {
    let mock = liberado_provider::MockProvider::new("mock");
    if let Some(text) = resolution {
        mock.push(CompletionResponse::text(text));
    }
    let typed = Arc::new(mock);
    (typed.clone(), typed)
}

fn child(branch: &str, tip: Option<&str>, outcome: Outcome) -> ChildOutcome {
    ChildOutcome {
        label: branch.into(),
        branch: branch.into(),
        tip_sha: tip.map(str::to_string),
        outcome,
        summary: String::new(),
        files_changed: vec![],
        session_id: None,
        error: None,
    }
}

#[tokio::test]
async fn a_child_without_a_tip_is_not_merged() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_repo(dir.path());
    let (_merger, mock) = merger_with(None);
    let children = vec![child("fanout/ghost", None, Outcome::Succeeded)];
    let report = finish_fanout(mock.as_ref(), &root, children).await.unwrap();
    assert_eq!(report.merges.len(), 1);
    assert!(
        report.merges[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("no branch tip")),
        "{report:?}"
    );
}

/// One failing child must fail the fan-out even when every merge is clean.
#[tokio::test]
async fn a_failed_child_fails_the_overall_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_repo(dir.path());
    let (_merger, mock) = merger_with(None);

    // A real branch with its own commit merges cleanly.
    let wt = add_worktree_on_branch(&root, root.parent().unwrap(), "clean-child", "fanout/clean")
        .await
        .unwrap();
    std::fs::write(wt.join("feature.txt"), "work\n").unwrap();
    git(&wt, &["add", "feature.txt"]);
    git(&wt, &["commit", "-m", "branch work", "--quiet"]);
    remove_worktree(&root, &wt).await.unwrap();

    let sha = String::from_utf8(
        liberado_common::process::std_command("git")
            .args(["-C", &root.to_string_lossy(), "rev-parse", "fanout/clean"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    // The branch itself merges cleanly; the CHILD still reported failure.
    let children = vec![child("fanout/clean", Some(&sha), Outcome::Failed)];
    let report = finish_fanout(mock.as_ref(), &root, children).await.unwrap();
    assert!(
        report.merges.iter().all(|m| m.error.is_none()),
        "{report:?}"
    );
    assert_eq!(
        report.overall,
        Outcome::Failed,
        "a failed child cannot ride on clean merges: {report:?}"
    );
}

#[test]
fn children_run_on_a_tight_turn_budget() {
    let subtask = CodingSubtask {
        label: "a".into(),
        description: "do a".into(),
        success_criteria: vec![],
    };
    let request = child_request(Path::new("/tmp/wt"), &subtask, "m");
    assert_eq!(
        request.config.progress.max_attempts, 2,
        "fan-out children get two attempts, not the default budget"
    );
}

#[tokio::test]
async fn llm_conflict_resolution_stages_and_commits_the_resolved_content() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_repo(dir.path());
    let wt_base = root.parent().unwrap().join("wts-llm");

    // Branch edits README to "theirs".
    let wt = add_worktree_on_branch(&root, &wt_base, "conflict-child", "fanout/conflict")
        .await
        .unwrap();
    std::fs::write(wt.join("README.md"), "theirs\n").unwrap();
    git(&wt, &["add", "README.md"]);
    git(&wt, &["commit", "-m", "branch edit", "--quiet"]);
    remove_worktree(&root, &wt).await.unwrap();

    // Parent edits README to "ours" — merging now conflicts.
    std::fs::write(root.join("README.md"), "ours\n").unwrap();
    git(&root, &["add", "README.md"]);
    git(&root, &["commit", "-m", "parent edit", "--quiet"]);

    match merge_branch(&root, "fanout/conflict")
        .await
        .expect("merge ran")
    {
        MergeAttempt::Conflicts { paths } => {
            let (merger, _mock) = merger_with(Some("resolved-content\n"));
            let merge_commit =
                resolve_conflicts_with_llm(merger.as_ref(), &root, "fanout/conflict", &paths)
                    .await
                    .expect("resolution commits");
            assert_eq!(
                merge_commit.len(),
                40,
                "a real merge commit sha comes back, not a placeholder"
            );
            assert_eq!(
                std::fs::read_to_string(root.join("README.md")).unwrap(),
                "resolved-content",
                "the scripted resolution is what landed (trimmed)"
            );
        }
        other => panic!("expected conflicts, got {other:?}"),
    }
}

#[tokio::test]
async fn resolved_content_has_fences_stripped() {
    let sides = liberedo_sides();
    let (merger, _mock) = merger_with(Some("```rust\nfn main() {}\n```\n"));
    let body = llm_resolve_file(merger.as_ref(), "fanout/x", &sides)
        .await
        .expect("resolution");
    assert_eq!(body, "fn main() {}", "{body}");
}

#[tokio::test]
async fn empty_resolution_is_an_error_not_an_empty_file() {
    let sides = liberedo_sides();
    let (merger, _mock) = merger_with(Some("   \n"));
    let err = llm_resolve_file(merger.as_ref(), "fanout/x", &sides)
        .await
        .expect_err("blank content must be refused");
    assert!(err.contains("empty content"), "{err}");
}

fn liberedo_sides() -> liberado_coder_sandbox::ConflictSides {
    liberado_coder_sandbox::ConflictSides {
        path: "README.md".into(),
        ours: "ours\n".into(),
        theirs: "theirs\n".into(),
        combined: "<<<<<<< ours\nours\n=======\ntheirs\n>>>>>>> theirs\n".into(),
    }
}
