//! Runner behavior end-to-end against real git and a recording forge stub — the whole
//! D1 loop minus the model: clone from a bare remote, worktree, run (stub backend
//! writes a file), commit, push, open PR. Every behavior claim here is mutation-testable:
//! break the step in runner.rs, watch its test fail, restore.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use liberado_coder_core::{CoderBackend, CoderError, CoderRunRequest, CoderRunResult};
use liberado_common::Outcome;
use liberado_delegate_contract::{
    Acceptance, TaskBudget, TaskGrant, TaskId, TaskSpec, TaskStatus, WorkerEvent,
};
use liberado_forge::{
    CheckStates, ForgeClient, ForgeError, MergeCommit, MergeMethod, OpenPr, PrRef,
};

use super::{RunContext, branch_name, slugify};
use crate::config::WorkerSettings;
use crate::queue::TaskStore;

// --- stubs ---------------------------------------------------------------

struct WriteFileBackend {
    outcome: Outcome,
    runs: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl CoderBackend for WriteFileBackend {
    fn name(&self) -> &str {
        "write-file-stub"
    }

    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        let n = self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let root = PathBuf::from(&request.workspace.root);
        tokio::fs::write(
            root.join("delivered.txt"),
            format!("work by {}, pass {}\n", request.task.id, n),
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
    comments: Arc<Mutex<Vec<(u64, String)>>>,
}

impl RecordingForge {
    fn opened(&self) -> Vec<OpenPr> {
        self.opened.lock().expect("forge lock").clone()
    }
}

/// Writes like [`WriteFileBackend`] but records every goal it is handed, so tests
/// can pin what the model actually saw (the kickback seeding lives there).
struct GoalCapturingBackend {
    goals: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl CoderBackend for GoalCapturingBackend {
    fn name(&self) -> &str {
        "goal-capturing-stub"
    }

    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        self.goals
            .lock()
            .expect("goals lock")
            .push(request.task.description.clone());
        let root = PathBuf::from(&request.workspace.root);
        tokio::fs::write(root.join("delivered.txt"), "work\n")
            .await
            .map_err(|error| CoderError::Backend(error.to_string()))?;
        Ok(CoderRunResult {
            backend: self.name().to_string(),
            outcome: Outcome::Succeeded,
            summary: "wrote delivered.txt".into(),
            files_changed: vec!["delivered.txt".into()],
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
    async fn comment(&self, pr: &PrRef, body: &str) -> Result<(), ForgeError> {
        self.comments
            .lock()
            .expect("comments lock")
            .push((pr.number, body.to_string()));
        Ok(())
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
    store: Arc<TaskStore>,
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

    let store = Arc::new(TaskStore::open(&root.path().join("data")).expect("store"));
    let settings = Arc::new(WorkerSettings {
        bind: "127.0.0.1:0".into(),
        token: "test-token".into(),
        data_dir: root.path().join("data"),
        config_dir: None,
        model: None,
        forge_url: Some("http://forge.example".into()),
        forge_token: "freshtoken".into(),
        forge_insecure_tls: false,
        clone_base_url: Some(root.path().join("remote").to_string_lossy().into_owned()),
        max_concurrent: 2,
        question_timeout_secs: 1,
        max_open_questions: 3,
    });
    Harness {
        root,
        remote,
        settings,
        store,
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
    backend: impl CoderBackend + 'static,
    forge: RecordingForge,
) -> RunContext {
    let store = Arc::new(TaskStore::open(&harness.settings.data_dir).expect("store"));
    RunContext {
        settings: harness.settings.clone(),
        store,
        backends: Arc::new(crate::runner::FixedBackend(Arc::new(backend))),
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
            runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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
    let expected_namespace = TaskId("01JTESTTASK000000".into()).short();
    assert!(
        branch.starts_with(&format!("delegate/{expected_namespace}/")),
        "branch namespaced by the task id's random tail, got {branch}"
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

    // The real execution path journals every transition with rising correlations.
    use liberado_delegate_contract::EventKind;
    let events = h.store.replay("01JTESTTASK000000").expect("replay");
    let kinds: Vec<EventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![
            EventKind::StatusChanged,
            EventKind::StatusChanged,
            EventKind::PrReady
        ],
        "{events:?}"
    );
    for pair in events.windows(2) {
        let seq = |e: &WorkerEvent| -> u64 {
            e.correlation_id
                .rsplit(':')
                .next()
                .unwrap()
                .parse()
                .expect("seq")
        };
        assert!(
            seq(&pair[1]) > seq(&pair[0]),
            "correlations increase: {events:?}"
        );
    }

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
            runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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
            runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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

// --- kickback (D3 slice A) ------------------------------------------------

#[tokio::test]
async fn kickback_reruns_on_the_same_branch_updates_the_pr_and_comments() {
    let harness = harness();
    let spec = spec("01KICKRUN000000000000TEST1");
    let forge = RecordingForge::default();
    let ctx = context(
        &harness,
        WriteFileBackend {
            outcome: Outcome::Succeeded,
            runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        },
        forge.clone(),
    )
    .await;
    ctx.store.submit(&spec).expect("submit");

    let first = super::execute(ctx.clone(), spec.clone()).await;
    assert!(
        matches!(first.status, TaskStatus::PrOpened { .. }),
        "{first:?}"
    );
    assert_eq!(forge.opened.lock().unwrap().len(), 1, "one PR so far");
    let branch = branch_name(&spec);
    let tip_sha = |remote: &Path| {
        String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args([
                    "-C",
                    remote.to_string_lossy().as_ref(),
                    "rev-parse",
                    &format!("refs/heads/{branch}"),
                ])
                .output()
                .expect("rev-parse runs")
                .stdout,
        )
        .to_string()
    };
    let tip_before = tip_sha(&harness.remote);

    // The answers layer journals the round before spawning; mirror that here.
    let round = ctx
        .store
        .record_instruction(&spec.id, "rename the file to kicker.md")
        .unwrap();

    let second = super::execute_kickback(
        ctx.clone(),
        spec.clone(),
        round,
        "rename the file to kicker.md".into(),
    )
    .await;
    assert!(
        matches!(second.status, TaskStatus::PrOpened { ref url } if Some(url) == first.pr_url.as_ref()),
        "same PR url after kickback: {second:?}"
    );
    assert_eq!(forge.opened.lock().unwrap().len(), 1, "no duplicate PR");
    let comments = forge.comments.lock().unwrap();
    assert_eq!(comments.len(), 1, "summary comment on the existing PR");
    assert_eq!(comments[0].0, 1);
    assert!(
        comments[0].1.contains("Kickback applied"),
        "{}",
        comments[0].1
    );
    drop(comments);
    let tip_after = tip_sha(&harness.remote);
    assert_ne!(tip_before, tip_after, "the re-run must push new work");

    // Journal shows PrOpened -> Running -> PrOpened across the round.
    let kinds: Vec<_> = ctx
        .store
        .replay(&spec.id.0)
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            liberado_delegate_contract::EventKind::StatusChanged, // queued
            liberado_delegate_contract::EventKind::StatusChanged, // running
            liberado_delegate_contract::EventKind::PrReady,       // pr 1
            liberado_delegate_contract::EventKind::StatusChanged, // running again
            liberado_delegate_contract::EventKind::PrReady,       // pr 2
        ]
    );
}

#[tokio::test]
async fn kickback_seeds_the_instruction_into_the_goal_the_model_sees() {
    let harness = harness();
    let spec = spec("01KICKRUN000000000000TEST2");
    let goals = Arc::new(Mutex::new(Vec::new()));
    let forge = RecordingForge::default();
    let ctx = context(
        &harness,
        GoalCapturingBackend {
            goals: goals.clone(),
        },
        forge,
    )
    .await;
    ctx.store.submit(&spec).expect("submit");
    let _ = super::execute(ctx.clone(), spec.clone()).await;
    let round = ctx
        .store
        .record_instruction(&spec.id, "use kebab-case everywhere")
        .unwrap();
    let _ = super::execute_kickback(ctx, spec, round, "use kebab-case everywhere".into()).await;

    let goals = goals.lock().unwrap();
    assert_eq!(goals.len(), 2);
    assert!(
        !goals[0].contains("Kickback"),
        "first pass sees the plain goal"
    );
    assert!(goals[1].contains("Kickback round 1"), "{}", goals[1]);
    assert!(
        goals[1].contains("use kebab-case everywhere"),
        "{}",
        goals[1]
    );
    assert!(
        goals[1].starts_with(&goals[0]),
        "original goal preserved as prefix"
    );
}

#[tokio::test]
async fn kickback_without_a_pr_fails_honestly_instead_of_running() {
    let harness = harness();
    let spec = spec("01KICKRUN000000000000TEST3");
    let forge = RecordingForge::default();
    let ctx = context(
        &harness,
        WriteFileBackend {
            outcome: Outcome::Succeeded,
            runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        },
        forge,
    )
    .await;
    // Submitted but never run: no PR exists to kick back against.
    ctx.store.submit(&spec).expect("submit");
    let record = super::execute_kickback(ctx.clone(), spec, 1, "nonsense".into()).await;
    match record.status {
        TaskStatus::Failed { reason } => {
            assert!(reason.contains("no open PR"), "{reason}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn pr_urls_from_our_own_responses_round_trip_into_references() {
    use super::pr::pr_ref_from_url;
    let repo = liberado_forge::RepoPath("o/r".into());
    let pr = pr_ref_from_url("https://forge.example/o/r/pulls/17", repo.clone()).expect("parses");
    assert_eq!(pr.number, 17);
    assert_eq!(pr.repo.api_segment(), "o/r");
    assert!(pr_ref_from_url("https://forge.example/o/r", repo).is_none());
}
