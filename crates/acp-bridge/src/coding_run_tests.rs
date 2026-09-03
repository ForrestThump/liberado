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
    let saved_global = std::env::var("GIT_CONFIG_GLOBAL").ok();
    // SAFETY: single-threaded under ENV_LOCK; prior value restored below.
    unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", empty_cfg.path()) };

    let result = preserve_worktree(repo.path(), "done").await;

    match saved_global {
        Some(v) => unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", v) },
        None => unsafe { std::env::remove_var("GIT_CONFIG_GLOBAL") },
    }

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
    let saved_data_dir = std::env::var("LIBERADO_DATA_DIR").ok();
    // SAFETY: single-threaded under ENV_LOCK; prior value restored below.
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

    match saved_data_dir {
        Some(v) => unsafe { std::env::set_var("LIBERADO_DATA_DIR", v) },
        None => unsafe { std::env::remove_var("LIBERADO_DATA_DIR") },
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
    assert!(
        step_from_json(&json!({ "name": "", "run": "exit 0" })).is_none(),
        "empty name must be rejected by the guard itself"
    );
    assert!(
        step_from_json(&json!({ "name": "n", "run": "" })).is_none(),
        "empty run must be rejected by the guard itself"
    );
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

/// `workspace_env` carries a real ordinary cache only. Blanks and absents stay
/// worktree-local. An exact `shared_target_dir` is trimmed and used as-is so C3
/// pins do not move.
#[test]
fn workspace_env_carries_only_a_real_shared_target_dir() {
    let workspace = tempfile::tempdir().unwrap();
    let mut tuning = CoderTuning::default();
    assert!(
        workspace_env(&tuning, workspace.path()).is_empty(),
        "nothing configured"
    );

    tuning.workspace_build.shared_target_dir = Some("  ".into());
    assert!(
        workspace_env(&tuning, workspace.path()).is_empty(),
        "blank is not a directory"
    );

    tuning.workspace_build.shared_target_dir = Some(" target/shared ".into());
    let env = workspace_env(&tuning, workspace.path());
    assert_eq!(
        env.get("CARGO_TARGET_DIR"),
        Some(&"target/shared".to_string()),
        "exact pin is trimmed and used as CARGO_TARGET_DIR"
    );

    let managed = workspace.path().join("managed");
    tuning.workspace_build.shared_target_dir = None;
    tuning.workspace_build.managed_target_root = Some(managed.to_string_lossy().into_owned());
    let env = workspace_env(&tuning, workspace.path());
    let dir = env.get("CARGO_TARGET_DIR").expect("managed ordinary cache");
    assert!(
        Path::new(dir).starts_with(&managed),
        "managed root must own the ordinary cache: {dir}"
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

// ── commit_and_branch ────────────────────────────────────────────────────────

fn listed_branches(repo: &std::path::Path) -> Vec<String> {
    let out = liberado_common::process::std_command("git")
        .args([
            "-C",
            &repo.to_string_lossy(),
            "for-each-ref",
            "refs/heads",
            "--format",
            "%(refname:short)",
        ])
        .output()
        .expect("git for-each-ref");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// A successful `checkout -b` must be reported as success: the remediation
/// path only proceeds when isolation actually happened.
#[tokio::test]
async fn commit_and_branch_reports_success_and_creates_the_branch() {
    let _guard = ENV_LOCK.lock().await;
    let repo = temp_repo();
    commit_and_branch(repo.path(), "agent/isolated-ok")
        .await
        .expect("checkout -b on a clean repo must succeed");
    assert!(
        listed_branches(repo.path())
            .iter()
            .any(|b| b == "agent/isolated-ok"),
        "the isolated branch must exist after the call"
    );
}

/// Outside a git repo there is no isolation, and pretending otherwise would
/// put a speculative fix on uncommitted work.
#[tokio::test]
async fn commit_and_branch_fails_outside_a_git_repo() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().expect("plain dir");
    let err = commit_and_branch(dir.path(), "agent/nowhere")
        .await
        .expect_err("a non-repo cannot host the branch");
    assert!(
        err.contains("not a git repository"),
        "the error must name the missing repo: {err}"
    );
}

// ── warm_workspace_if_configured ─────────────────────────────────────────────

/// A manifest cargo rejects immediately — no compilation, fails in well under
/// a second, and proves whether the tree was consulted at all.
fn broken_manifest_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "not [valid toml").expect("write manifest");
    dir
}

/// With warmup off the workspace is never touched. Pointing it at a broken
/// manifest proves that: had the mutant run cargo, the baseline check would
/// refuse and this would return Err.
#[tokio::test]
async fn a_disabled_warmup_never_consults_the_tree() {
    let dir = broken_manifest_workspace();
    let mut tuning = CoderTuning::default();
    tuning.workspace_build.warmup = false;
    warm_workspace_if_configured(&tuning, dir.path())
        .await
        .expect("warmup=false must return without inspecting anything");
}

/// With warmup on, a baseline that does not compile refuses the run before
/// the model spends a token.
#[tokio::test]
async fn an_enabled_warmup_refuses_a_tree_that_does_not_compile() {
    let dir = broken_manifest_workspace();
    let mut tuning = CoderTuning::default();
    tuning.workspace_build.warmup = true;
    let err = warm_workspace_if_configured(&tuning, dir.path())
        .await
        .expect_err("a broken baseline must refuse the run");
    assert!(
        err.contains("does not compile"),
        "the refusal must say why: {err}"
    );
}

// ── remediate_if_needed ──────────────────────────────────────────────────────

use liberado_coder_core::{Remedy, SessionFinding};

fn actionable_finding() -> SessionFinding {
    SessionFinding {
        kind: "abandoned_finding".into(),
        quote: "all checks pass".into(),
        why: "no check was ever run".into(),
        remedy: Remedy::Repair,
    }
}

fn remediation_backend() -> LiberadoLoopBackend {
    LiberadoLoopBackend::new(Arc::new(liberado_provider::MockProvider::new("mock")))
}

fn base_request_for(workspace: &std::path::Path) -> liberado_coder_core::CoderRunRequest {
    let task = CoderTask::new("remediation-guard-test", "fix the findings");
    let surface = assemble_production_run(
        &CoderTuning::default(),
        liberado_coder_agent::assemble::entry::acp_surface(
            task,
            workspace.to_path_buf(),
            None,
            Some(1),
            0,
            Vec::new(),
        ),
    );
    surface.request
}

fn has_remediation_branch(repo: &std::path::Path) -> bool {
    listed_branches(repo)
        .iter()
        .any(|b| b.starts_with("agent/remediation-"))
}

/// Remediation switched on with something actionable must isolate first:
/// the branch existing (even though the mock provider immediately runs dry)
/// is the observable half of the guard. A body that returns early never
/// creates one.
#[tokio::test]
async fn enabled_remediation_with_findings_isolates_a_branch() {
    let _guard = ENV_LOCK.lock().await;
    let repo = temp_repo();
    let mut tuning = CoderTuning::default();
    tuning.session_critic.remediation = true;
    let request = base_request_for(repo.path());
    let record = remediate_if_needed(
        &remediation_backend(),
        &tuning,
        "sess-iso-on",
        repo.path(),
        &request,
        &[actionable_finding()],
    )
    .await;
    assert!(
        record.is_none(),
        "an exhausted mock produces no record; got {record:?}"
    );
    assert!(
        has_remediation_branch(repo.path()),
        "the isolation branch must exist even when the fix itself failed: {:?}",
        listed_branches(repo.path())
    );
}

/// Remediation switched off must not touch the tree at all. Under the
/// inverted guard a disabled pass still isolates a branch.
#[tokio::test]
async fn disabled_remediation_never_isolates_a_branch() {
    let _guard = ENV_LOCK.lock().await;
    let repo = temp_repo();
    let mut tuning = CoderTuning::default();
    tuning.session_critic.remediation = false;
    let request = base_request_for(repo.path());
    let record = remediate_if_needed(
        &remediation_backend(),
        &tuning,
        "sess-iso-off",
        repo.path(),
        &request,
        &[actionable_finding()],
    )
    .await;
    assert!(record.is_none(), "disabled means no record: {record:?}");
    assert!(
        !has_remediation_branch(repo.path()),
        "disabled remediation must not create branches: {:?}",
        listed_branches(repo.path())
    );
}

/// Enabled but nothing to act on: skipping happens before any isolation,
/// so no branch may appear.
#[tokio::test]
async fn enabled_remediation_without_findings_skips_before_isolation() {
    let _guard = ENV_LOCK.lock().await;
    let repo = temp_repo();
    let mut tuning = CoderTuning::default();
    tuning.session_critic.remediation = true;
    let request = base_request_for(repo.path());
    let record = remediate_if_needed(
        &remediation_backend(),
        &tuning,
        "sess-iso-empty",
        repo.path(),
        &request,
        &[],
    )
    .await;
    assert!(record.is_none(), "nothing to act on: {record:?}");
    assert!(
        !has_remediation_branch(repo.path()),
        "an empty finding list must not reach the branch step: {:?}",
        listed_branches(repo.path())
    );
}
