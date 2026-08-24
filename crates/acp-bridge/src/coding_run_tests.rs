//! Split from `coding_run.rs` for module-health boundaries.

use super::*;
/// Process-global env mutations in this crate must not race other tests.
///
/// `tokio::sync::Mutex`, not `std::sync::Mutex`: these tests hold the guard across an `.await`,
/// which `clippy::await_holding_lock` rejects for a blocking lock — it parks the whole runtime
/// thread rather than yielding. `coder-agent`'s `DATA_DIR_ENV_LOCK` is the same pattern for the
/// same reason. (Test binaries are per-crate, so this cannot be the *same* lock, only the same
/// shape.)
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A git repo with one commit, built without relying on any global git identity.
///
/// The identity is passed with `-c` here for the same reason `preserve_worktree` does it:
/// `user.email` / `user.name` exist on every dev machine and on no CI runner, so a fixture
/// that leans on global config passes locally and fails in CI.
fn temp_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().to_string_lossy().to_string();
    let run = |args: &[&str]| {
        let out = liberado_common::process::std_command("git")
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["-C", &p, "init", "-q"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("seed");
    run(&["-C", &p, "add", "-A"]);
    run(&[
        "-C",
        &p,
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "seed",
    ]);
    dir
}

fn is_dirty(repo: &std::path::Path) -> bool {
    let out = liberado_common::process::std_command("git")
        .args(["-C", &repo.to_string_lossy(), "status", "--porcelain"])
        .output()
        .expect("git status");
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

/// The whole point: a run's output must survive without anyone remembering to commit it.
///
/// Runs with `GIT_CONFIG_GLOBAL` pointed at an empty file, which is the CI condition — no
/// `user.name`, no `user.email`. Without the `-c` flags in `preserve_worktree` this fails
/// with "Please tell me who you are", which is precisely the failure that passes on a
/// developer box and breaks on a runner.
#[tokio::test]
async fn a_dirty_worktree_is_committed_even_with_no_global_git_identity() {
    let _guard = ENV_LOCK.lock().await;
    let repo = temp_repo();
    std::fs::write(repo.path().join("work.txt"), "agent output\n").expect("write");
    assert!(is_dirty(repo.path()), "precondition: tree must be dirty");

    let empty_cfg = tempfile::NamedTempFile::new().expect("cfg");
    // SAFETY: single-threaded under ENV_LOCK; removed below.
    unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", empty_cfg.path()) };

    let result = preserve_worktree(repo.path(), "done").await;

    unsafe { std::env::remove_var("GIT_CONFIG_GLOBAL") };

    let sha = result
        .expect("preserving a dirty worktree must succeed without global identity")
        .expect("a dirty tree must produce a commit");
    assert!(!sha.is_empty(), "commit sha must be reported");
    assert!(
        !is_dirty(repo.path()),
        "the tree must be clean after preservation - nothing left to lose"
    );
}

/// A clean tree must not manufacture an empty commit, or every prompt adds noise to history.
#[tokio::test]
async fn a_clean_worktree_produces_no_commit() {
    let _guard = ENV_LOCK.lock().await;
    let repo = temp_repo();
    assert!(!is_dirty(repo.path()), "precondition: tree must be clean");

    let preserved = preserve_worktree(repo.path(), "done")
        .await
        .expect("a clean tree is not an error");
    assert!(
        preserved.is_none(),
        "a clean tree must report nothing preserved, got {preserved:?}"
    );
}

#[tokio::test]
async fn prepare_workspace_fails_hard_when_worktree_setup_fails() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().expect("tempdir");
    // Looks like a git repo to is_git_repo, but is not a real repo → worktree create fails.
    std::fs::create_dir(dir.path().join(".git")).expect(".git");

    let data = tempfile::tempdir().expect("data dir");
    // SAFETY: single-threaded under env_lock; restored below.
    unsafe {
        std::env::set_var("LIBERADO_DATA_DIR", data.path());
    }

    let err = prepare_workspace(dir.path(), "sess-hard-fail")
        .await
        .expect_err("must not fall back to host cwd");
    assert!(
        err.contains("durable session worktree"),
        "error should name worktree setup, got: {err}"
    );
    assert!(
        err.contains("no live-tree fallback"),
        "error should refuse live-tree demotion, got: {err}"
    );

    unsafe {
        std::env::remove_var("LIBERADO_DATA_DIR");
    }
}

#[tokio::test]
async fn prepare_workspace_non_git_uses_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = prepare_workspace(dir.path(), "sess-nongit")
        .await
        .expect("non-git host cwd is ok");
    assert_eq!(path, dir.path());
}

/// A step missing only its name must be dropped; a step missing only its run
/// must also be dropped. `&&` would keep half-defined steps.
#[test]
fn step_from_json_requires_both_name_and_run() {
    assert!(step_from_json(&json!({ "name": "only-name" })).is_none());
    assert!(step_from_json(&json!({ "run": "exit 0" })).is_none());
    let full = step_from_json(&json!({ "name": "n", "run": "exit 0" })).expect("complete");
    assert_eq!(full.name, "n");
}

/// The workspace payload is what an operator reads to answer "where did this run".
/// An empty object would say nothing.
#[test]
fn workspace_payload_names_the_root() {
    let p = workspace_payload(Path::new("/somewhere/ws"));
    assert_eq!(p["workspace_root"], "/somewhere/ws");
}

/// Files changed render as their own section; an empty list renders none.
#[test]
fn render_lists_files_changed_only_when_there_are_any() {
    let mk = |files: Vec<&str>| CodingRoundOutcome {
        summary: String::new(),
        outcome: "Succeeded".into(),
        files_changed: files.iter().map(|s| s.to_string()).collect(),
        workspace: "/tmp/ws".into(),
        trace_path: None,
        validation_notes: None,
        findings: String::new(),
    };
    let with = mk(vec!["src/a.rs"]).render();
    assert!(with.contains("**Files changed:**"), "{with}");
    assert!(with.contains("- `src/a.rs`"), "{with}");
    let without = mk(vec![]).render();
    assert!(
        !without.contains("Files changed"),
        "an empty list must not print the heading: {without}"
    );
}

/// `workspace_env` carries the shared build cache to children verbatim; blanks
/// and absents contribute nothing. (Trimming is apply_shared_target_dir's job.)
#[test]
fn workspace_env_carries_only_a_real_shared_target_dir() {
    let mut tuning = CoderTuning::default();
    assert!(workspace_env(&tuning).is_empty(), "nothing configured");

    tuning.workspace_build.shared_target_dir = Some("  ".into());
    assert!(
        workspace_env(&tuning).is_empty(),
        "blank is not a directory"
    );

    tuning.workspace_build.shared_target_dir = Some(" target/shared ".into());
    let env = workspace_env(&tuning);
    assert_eq!(
        env.get("CARGO_TARGET_DIR"),
        Some(&" target/shared ".to_string()),
        "passed through untouched; downstream trims"
    );
}

/// `[acp]` from real config reaches the caller; a broken load falls back to defaults.
#[test]
fn load_acp_config_reads_the_declared_section_and_defaults_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("topology.toml"),
        "vault_path = \"/tmp/vault\"\n\n[acp]\nmax_turns = 7\n",
    )
    .unwrap();
    let cfg = load_acp_config(Some(dir.path()));
    assert_eq!(
        cfg.max_turns,
        Some(7),
        "[acp] max_turns must reach the bridge"
    );

    // A dir without topology falls back to defaults rather than erroring.
    let empty = tempfile::tempdir().unwrap();
    let cfg = load_acp_config(Some(empty.path()));
    assert_eq!(cfg.max_turns, None);
}
