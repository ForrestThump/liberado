//! Runner behavior end-to-end against real git and a recording forge stub — the whole
//! D1 loop minus the model: clone from a bare remote, worktree, run (stub backend
//! writes a file), commit, push, open PR. Every behavior claim here is mutation-testable:
//! break the step in runner.rs, watch its test fail, restore.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use liberado_coder_core::{CoderBackend, CoderError, CoderRunRequest, CoderRunResult};
use liberado_common::Outcome;
use liberado_delegate_contract::{Acceptance, TaskBudget, TaskGrant, TaskId, TaskSpec, TaskStatus};
use liberado_forge::{
    CheckStates, ForgeClient, ForgeError, MergeCommit, MergeMethod, OpenPr, PrRef,
};

use super::{RunContext, branch_name, slugify};
use crate::config::WorkerSettings;
use crate::queue::TaskStore;

// --- stubs ---------------------------------------------------------------

struct WriteFileBackend {
    outcome: Outcome,
}

#[async_trait::async_trait]
impl CoderBackend for WriteFileBackend {
    fn name(&self) -> &str {
        "write-file-stub"
    }

    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        let root = PathBuf::from(&request.workspace.root);
        tokio::fs::write(
            root.join("delivered.txt"),
            format!("work by {}\n", request.task.id),
        )
        .await
        .map_err(|error| CoderError::Backend(error.to_string()))?;
        // The pack writes its traces into the worktree; the branch must not carry them.
        let traces = root.join("coder-traces");
        tokio::fs::create_dir_all(&traces)
            .await
            .map_err(|error| CoderError::Backend(error.to_string()))?;
        tokio::fs::write(traces.join("session.json"), "{}\n")
            .await
            .map_err(|error| CoderError::Backend(error.to_string()))?;
        Ok(CoderRunResult {
            backend: self.name().to_string(),
            outcome: self.outcome,
            summary: "wrote delivered.txt".into(),
            files_changed: vec!["delivered.txt".into(), "coder-traces/session.json".into()],
            file_changes: vec![],
            validation_notes: None,
            critic_verdict: None,
            gate_votes: vec![],
            trace_path: None,
            diff_findings: vec![],
            session_findings: vec![],
            remediation: None,
            diagnostics: serde_json::Value::Null,
        })
    }
}

#[derive(Default, Clone)]
struct RecordingForge {
    opened: Arc<Mutex<Vec<OpenPr>>>,
}

impl RecordingForge {
    fn opened(&self) -> Vec<OpenPr> {
        self.opened.lock().expect("forge lock").clone()
    }
}

#[async_trait::async_trait]
impl ForgeClient for RecordingForge {
    async fn open_pr(&self, req: &OpenPr) -> Result<PrRef, ForgeError> {
        self.opened.lock().expect("forge lock").push(req.clone());
        Ok(PrRef {
            repo: req.repo.clone(),
            number: 1,
            url: format!("http://forge.example/{}/pulls/1", req.repo.api_segment()),
        })
    }
    async fn comment(&self, _pr: &PrRef, _body: &str) -> Result<(), ForgeError> {
        Err(ForgeError::Shape("not used in D1 tests".into()))
    }
    async fn checks(&self, _pr: &PrRef, names: &[String]) -> Result<CheckStates, ForgeError> {
        Ok(CheckStates {
            overall: liberado_forge::CheckState::Success,
            named: names
                .iter()
                .map(|n| (n.clone(), liberado_forge::CheckState::Success))
                .collect(),
        })
    }
    async fn merge(&self, _pr: &PrRef, _method: MergeMethod) -> Result<MergeCommit, ForgeError> {
        Err(ForgeError::Shape("workers cannot merge".into()))
    }
}

// --- harness -------------------------------------------------------------

fn sh(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@test"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Bare remote seeded with one commit on `main`, plus worker settings pointing at it.
struct Harness {
    #[allow(dead_code)]
    root: tempfile::TempDir,
    remote: PathBuf,
    settings: Arc<WorkerSettings>,
}

fn harness() -> Harness {
    let root = tempfile::tempdir().expect("tempdir");
    let remote_parent = root.path().join("remote").join("local");
    std::fs::create_dir_all(&remote_parent).expect("remote parent");
    let remote = remote_parent.join("repo.git");

    // Seed repo with a commit on main, pushed to the bare remote.
    let seed = root.path().join("seed");
    std::fs::create_dir_all(&seed).expect("seed dir");
    let run = |dir: &Path, args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "# seed\n").expect("seed file");
    sh(&seed, &["add", "-A"]);
    sh(&seed, &["commit", "-q", "-m", "seed"]);
    run(&seed, &["init", "-q", "--bare", &remote.to_string_lossy()]);
    run(&seed, &["push", "-q", &remote.to_string_lossy(), "main"]);

    let settings = Arc::new(WorkerSettings {
        bind: "127.0.0.1:0".into(),
        token: "test-token".into(),
        data_dir: root.path().join("data"),
        config_dir: None,
        model: None,
        forge_url: Some("http://forge.example".into()),
        forge_token: "freshtoken".into(),
        clone_base_url: Some(root.path().join("remote").to_string_lossy().into_owned()),
        max_concurrent: 2,
    });
    Harness {
        root,
        remote,
        settings,
    }
}

fn spec(goal: &str) -> TaskSpec {
    TaskSpec {
        id: TaskId("01JTESTTASK000000".into()),
        project: "demo".into(),
        repository: "local/repo".into(),
        base_branch: "main".into(),
        goal: goal.into(),
        success_criteria: vec!["a file appears".into()],
        acceptance: Acceptance::default(),
        budget: TaskBudget::default(),
        grant: TaskGrant::default(),
    }
}

async fn context(
    harness: &Harness,
    backend: WriteFileBackend,
    forge: RecordingForge,
) -> RunContext {
    let store = Arc::new(TaskStore::open(&harness.settings.data_dir).expect("store"));
    RunContext {
        settings: harness.settings.clone(),
        store,
        backend: Arc::new(backend),
        forge: Some(Arc::new(forge)),
    }
}

fn branch_tip(remote: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(remote)
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Every path in the branch's tree, one `ls-tree` line per file.
fn branch_tree(remote: &Path, branch: &str) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(remote)
        .args(["ls-tree", "-r", "--name-only", branch])
        .output()
        .expect("git ls-tree runs");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// --- the pipeline --------------------------------------------------------

/// The D1 acceptance path: a succeeded pack run lands a pushed branch and an open PR.
/// Break any step in `prepare_and_run` and this fails.
#[tokio::test]
async fn a_succeeded_run_pushes_a_delegate_branch_and_opens_the_pr() {
    let h = harness();
    let forge = RecordingForge::default();
    let ctx = context(
        &h,
        WriteFileBackend {
            outcome: Outcome::Succeeded,
        },
        forge.clone(),
    )
    .await;
    ctx.store
        .submit(&spec("fix the flaky widget"))
        .expect("submit");

    let record = super::execute(ctx, spec("fix the flaky widget")).await;

    let url = match &record.status {
        TaskStatus::PrOpened { url } => url.clone(),
        other => panic!("expected PrOpened, got {other:?}"),
    };
    assert!(url.ends_with("/pulls/1"));

    let branch = branch_name(&spec("fix the flaky widget"));
    assert!(
        branch.starts_with("delegate/01jtestt/"),
        "branch namespaced by task short id, got {branch}"
    );
    assert!(branch.ends_with(&slugify("fix the flaky widget")));
    assert!(
        branch_tip(&h.remote, &branch),
        "pushed branch exists on the remote"
    );

    // The PR body carries identity + criteria + summary.
    let opened = forge.opened();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].head, branch);
    assert_eq!(opened[0].base, "main");
    assert!(opened[0].body.contains("01JTESTTASK000000"));
    assert!(opened[0].body.contains("- [ ] a file appears"));
    assert!(opened[0].body.contains("wrote delivered.txt"));
    // The body describes the deliverable, not worker bookkeeping.
    assert!(
        !opened[0].body.contains("coder-traces"),
        "PR body must not list trace files"
    );
    assert!(opened[0].body.contains("- Files changed: 1"));

    // Worktree persists for inspection — including this run's traces; the record
    // carries session + PR.
    let worktree = h.settings.worktrees_dir().join("01JTESTTASK000000");
    assert!(worktree.exists());
    assert!(
        worktree.join("coder-traces/session.json").exists(),
        "traces stay on the worker even though they are not committed"
    );
    assert_eq!(record.pr_url.as_deref(), Some(url.as_str()));
    assert!(record.session_id.is_some());

    // The pushed branch carries the deliverable and not the traces.
    let tree = branch_tree(&h.remote, &branch);
    assert!(tree.contains("delivered.txt"));
    assert!(
        !tree.contains("coder-traces"),
        "plan §16: traces must not travel on the branch"
    );
}

/// A failed pack run is an honest failure: no push, no PR, worktree kept.
#[tokio::test]
async fn a_failed_run_records_failure_without_opening_anything() {
    let h = harness();
    let forge = RecordingForge::default();
    let ctx = context(
        &h,
        WriteFileBackend {
            outcome: Outcome::Failed,
        },
        forge.clone(),
    )
    .await;
    ctx.store.submit(&spec("hopeless task")).expect("submit");

    let record = super::execute(ctx, spec("hopeless task")).await;

    match &record.status {
        TaskStatus::Failed { reason } => assert!(reason.contains("Failed")),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(forge.opened().is_empty(), "no PR for a failed run");
    let branch = branch_name(&spec("hopeless task"));
    assert!(!branch_tip(&h.remote, &branch));
    assert!(
        h.settings
            .worktrees_dir()
            .join("01JTESTTASK000000")
            .exists()
    );
}

/// A missing base branch fails at worktree creation with a reason naming the step.
#[tokio::test]
async fn an_unknown_base_branch_fails_honestly() {
    let h = harness();
    let mut bad = spec("whatever");
    bad.base_branch = "no-such-branch".into();
    let ctx = context(
        &h,
        WriteFileBackend {
            outcome: Outcome::Succeeded,
        },
        RecordingForge::default(),
    )
    .await;
    ctx.store.submit(&bad).expect("submit");

    let record = super::execute(ctx, bad).await;
    match &record.status {
        TaskStatus::Failed { reason } => assert!(reason.contains("worktree"), "{reason}"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

// --- pure helpers --------------------------------------------------------

#[test]
fn slugify_is_ref_safe_and_bounded() {
    assert_eq!(slugify("Fix the Flaky Widget!"), "fix-the-flaky-widget");
    assert_eq!(
        slugify("  --leading and---trailing--  "),
        "leading-and-trailing"
    );
    assert!(slugify(&"x".repeat(100)).len() <= 40);
}

#[test]
fn branch_name_honors_the_grant_namespace() {
    let mut s = spec("do things");
    s.grant.branch_namespace = Some("bench-box".into());
    assert_eq!(branch_name(&s), "delegate/bench-box/do-things");
}

fn result_with_files(files: Vec<&str>) -> liberado_coder_core::CoderRunResult {
    liberado_coder_core::CoderRunResult {
        backend: "stub".into(),
        outcome: Outcome::Succeeded,
        summary: "did things".into(),
        files_changed: files.into_iter().map(str::to_string).collect(),
        file_changes: vec![],
        validation_notes: None,
        critic_verdict: None,
        gate_votes: vec![],
        trace_path: None,
        diff_findings: vec![],
        session_findings: vec![],
        remediation: None,
        diagnostics: serde_json::Value::Null,
    }
}

#[test]
fn pr_body_lists_deliverables_not_worker_bookkeeping() {
    let result = result_with_files(vec![
        "src/lib.rs",
        "coder-traces/session.json",
        ".liberado/offload/big.bin",
    ]);
    let body = super::pr_body(&spec("body test"), &result);
    assert!(body.contains("- Files changed: 1"), "{body}");
    assert!(body.contains("`src/lib.rs`"));
    assert!(!body.contains("coder-traces"), "{body}");
    assert!(!body.contains(".liberado"), "{body}");
}
