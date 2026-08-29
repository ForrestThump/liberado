//! Stable branch and directory names for fan-out children.

use sha2::{Digest, Sha256};

/// Sanitize a label for branch and worktree directory segments.
pub fn sanitize_label(label: &str) -> String {
    let sanitized: String = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('-').to_string();
    if sanitized.is_empty() {
        "task".into()
    } else {
        sanitized.chars().take(40).collect()
    }
}

/// Return branch and directory names scoped to one parent session.
///
/// Labels and child indexes repeat across goals. The session segment prevents a later goal from
/// deleting an active child's worktree or branch when it starts the same fan-out shape.
pub(super) fn fanout_child_names(
    parent_session_id: &str,
    label: &str,
    index: usize,
) -> (String, String) {
    let readable: String = sanitize_label(parent_session_id).chars().take(16).collect();
    let digest = Sha256::digest(parent_session_id.as_bytes());
    let namespace = format!(
        "{readable}-{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4]
    );
    let label = sanitize_label(label);
    (
        format!("fanout/{namespace}/{label}-{index}"),
        format!("fanout-{namespace}-{label}-{index}"),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use liberado_coder_sandbox::{add_worktree_on_branch, branch_tip, remove_worktree};

    use super::*;

    #[test]
    fn sanitize_preserves_alphanumeric_and_dash() {
        assert_eq!(sanitize_label("hello-world"), "hello-world");
        assert_eq!(sanitize_label("api_v2"), "api_v2");
        assert_eq!(sanitize_label("task-42"), "task-42");
    }

    #[test]
    fn sanitize_replaces_spaces_and_special_chars() {
        assert_eq!(sanitize_label("hello world"), "hello-world");
        assert_eq!(sanitize_label("fix: bug"), "fix--bug");
        assert_eq!(sanitize_label("a@b#c"), "a-b-c");
    }

    #[test]
    fn sanitize_trims_leading_trailing_dashes() {
        assert_eq!(sanitize_label("-hello-"), "hello");
        assert_eq!(sanitize_label("--start"), "start");
        assert_eq!(sanitize_label("end--"), "end");
    }

    #[test]
    fn sanitize_empty_returns_task() {
        assert_eq!(sanitize_label(""), "task");
        assert_eq!(sanitize_label("---"), "task");
        assert_eq!(sanitize_label("!!!%#"), "task");
    }

    #[test]
    fn sanitize_truncates_to_40_chars() {
        let long = "a".repeat(100);
        let out = sanitize_label(&long);
        assert_eq!(out.len(), 40);
        assert!(out.chars().all(|character| character == 'a'));
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        for args in [
            ["init", "--quiet"].as_slice(),
            ["config", "user.email", "test@liberado.local"].as_slice(),
            ["config", "user.name", "test"].as_slice(),
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(dir.join("README.md"), "base\n").unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["add", "README.md"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "-m", "base", "--quiet"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn names_are_stable_and_parent_scoped() {
        let first = fanout_child_names("session-a", "alpha", 0);
        assert_eq!(first, fanout_child_names("session-a", "alpha", 0));
        assert_ne!(first, fanout_child_names("session-b", "alpha", 0));
        assert_ne!(first, fanout_child_names("session-a", "alpha", 1));
    }

    /// A second goal with the same child shape must not replace the first goal's live branch.
    /// The old `{label}-{index}` names let the second add prune and delete `first_tip`.
    #[tokio::test]
    async fn same_child_shape_in_two_sessions_keeps_both_live_branches() {
        let root = tempfile::tempdir().unwrap();
        init_repo(root.path());
        let worktrees = root.path().join("coding-worktrees");

        let (branch_a, name_a) = fanout_child_names("session-a", "alpha", 0);
        let first = add_worktree_on_branch(root.path(), &worktrees, &name_a, &branch_a)
            .await
            .expect("first child worktree");
        assert!(
            std::process::Command::new("git")
                .args(["commit", "--allow-empty", "-m", "first child", "--quiet"])
                .current_dir(&first)
                .status()
                .unwrap()
                .success()
        );
        let first_tip = branch_tip(root.path(), &branch_a).await.unwrap();

        let (branch_b, name_b) = fanout_child_names("session-b", "alpha", 0);
        let second = add_worktree_on_branch(root.path(), &worktrees, &name_b, &branch_b)
            .await
            .expect("second child worktree");

        assert_ne!(first, second, "sessions must not share a worktree path");
        assert!(first.join(".git").exists(), "first child must stay live");
        assert_eq!(
            branch_tip(root.path(), &branch_a).await.unwrap(),
            first_tip,
            "starting the second goal must not reset the first child's branch"
        );

        remove_worktree(root.path(), &second).await.unwrap();
        remove_worktree(root.path(), &first).await.unwrap();
    }
}
