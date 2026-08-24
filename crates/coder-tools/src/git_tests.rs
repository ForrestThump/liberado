//! Split from `git.rs` for module-health boundaries.

use super::*;
/// Seed a repo with env identity only — no `user.name` in the repo config.
/// Isolated open must then see no committer; that is the CI runner's world.
fn repo_without_user_config() -> tempfile::TempDir {
    use liberado_common::process::std_command;
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = std_command("git")
            .args(args)
            .current_dir(dir.path())
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "--quiet"]);
    std::fs::write(dir.path().join("seed.txt"), "initial\n").unwrap();
    run(&["add", "seed.txt"]);
    run(&["commit", "-m", "initial"]);
    dir
}

#[test]
fn isolated_open_has_no_committer_without_fallbacks() {
    let dir = repo_without_user_config();
    let repo = gix::open_opts(dir.path(), gix::open::Options::isolated()).unwrap();
    assert!(
        repo.committer().is_none(),
        "fixture must not carry a committer or the identity fallback is untested"
    );
}

#[test]
fn branch_create_works_without_host_identity() {
    let dir = repo_without_user_config();
    let repo = open_repo_with(dir.path(), gix::open::Options::isolated()).unwrap();
    assert!(
        repo.committer().is_some(),
        "open_repo_with must install the agent identity when the host has none"
    );
    branch_create_in(&repo, "feature-x").expect("branch create without host identity");
}

/// Every IndexWorktree summary maps to its porcelain code; a stat-only update (None) maps to
/// nothing rather than a fake entry.
#[test]
fn index_worktree_codes_match_the_porcelain_table() {
    use gix::status::index_worktree::iter::Summary as S;
    assert_eq!(index_worktree_code(Some(S::Added)), Some("??"));
    assert_eq!(index_worktree_code(Some(S::Modified)), Some(" M"));
    assert_eq!(index_worktree_code(Some(S::Removed)), Some(" D"));
    assert_eq!(index_worktree_code(Some(S::TypeChange)), Some(" T"));
    assert_eq!(index_worktree_code(Some(S::Renamed)), Some(" R"));
    assert_eq!(index_worktree_code(Some(S::Copied)), Some(" C"));
    assert_eq!(index_worktree_code(Some(S::IntentToAdd)), Some(" A"));
    assert_eq!(index_worktree_code(Some(S::Conflict)), Some("UU"));
    assert_eq!(index_worktree_code(None), None);
}
