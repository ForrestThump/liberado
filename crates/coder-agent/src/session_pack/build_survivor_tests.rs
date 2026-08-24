//! Split from `session_pack/build.rs`: kills the baseline campaign's survivors.
//!
//! Covers git-repo detection (including the `.git` directory itself), the
//! fan-out nesting guard, mode-aware turn bounds, the restricted-mode notice,
//! guidance answer parsing, and contract integrity enforcement.

use super::super::tests::RestoreLiberadoDataDir;
use super::*;
use crate::{CoderBackend, CoderRunResult};
use liberado_coder_core::CoderError;
use liberado_session::{GoalSessionRecord, GoalSessionStore, HumanInput, SessionRecordStore};

#[test]
fn a_git_work_tree_is_recognised() {
    let dir = tempfile::tempdir().unwrap();
    let out = liberado_common::process::std_command("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(super::is_git_repo(dir.path()));
}

/// `rev-parse --is-inside-work-tree` answers exit-0 "false" from inside `.git`.
/// That must read as NOT a work tree — the conjunction matters.
#[test]
fn the_git_directory_itself_is_not_a_work_tree() {
    let dir = tempfile::tempdir().unwrap();
    let out = liberado_common::process::std_command("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!super::is_git_repo(&dir.path().join(".git")));
}

#[test]
fn a_plain_directory_is_not_a_git_repo() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!super::is_git_repo(dir.path()));
}

fn pack() -> CodingSessionPack {
    CodingSessionPack::new(
        Arc::new(liberado_provider::MockProvider::new("mock")),
        std::env::temp_dir(),
    )
}

fn policies(mode: CodingMode) -> WorkspacePolicies {
    use liberado_coder_core::{CommandPolicy, HashlineConfig, PathPolicy};
    WorkspacePolicies {
        path_policy: PathPolicy::default(),
        command_policy: CommandPolicy::default(),
        mode,
        hashline: HashlineConfig::default(),
    }
}

fn goal_with_max_turns(max_turns: u32) -> GoalSpec {
    serde_json::from_value(serde_json::json!({
        "description": "build it",
        "max_turns": max_turns
    }))
    .unwrap()
}

#[test]
fn explore_mode_bounds_turns_at_ten() {
    let pack = pack();
    let policies = policies(CodingMode::Explore);
    assert_eq!(
        pack.resolve_max_turns(&policies, &goal_with_max_turns(0)),
        10
    );
    assert_eq!(
        pack.resolve_max_turns(&policies, &goal_with_max_turns(25)),
        10,
        "exploration is capped at the preset even when the goal asks for more"
    );
    assert_eq!(
        pack.resolve_max_turns(&policies, &goal_with_max_turns(5)),
        5,
        "a smaller goal budget is honoured"
    );
}

#[test]
fn plan_mode_bounds_turns_at_eight() {
    let pack = pack();
    let policies = policies(CodingMode::Plan);
    assert_eq!(
        pack.resolve_max_turns(&policies, &goal_with_max_turns(0)),
        8
    );
    assert_eq!(
        pack.resolve_max_turns(&policies, &goal_with_max_turns(40)),
        8
    );
    assert_eq!(
        pack.resolve_max_turns(&policies, &goal_with_max_turns(3)),
        3
    );
}

#[test]
fn normal_mode_honours_the_goal_then_the_role_default() {
    let role_max_turns: u32 = 21;
    let pack = pack().with_coder_role(CoderRoleConfig {
        max_turns: Some(role_max_turns),
        ..CoderRoleConfig::default()
    });
    let policies = policies(CodingMode::Normal);
    assert_eq!(
        pack.resolve_max_turns(&policies, &goal_with_max_turns(30)),
        30
    );
    let fallback = pack.resolve_max_turns(&policies, &goal_with_max_turns(0));
    assert_eq!(
        fallback, role_max_turns,
        "with no goal budget, the pack's coder role ceiling applies"
    );
}

async fn events_channel() -> (
    tokio::sync::mpsc::Sender<SessionEvent>,
    tokio::sync::mpsc::Receiver<SessionEvent>,
) {
    tokio::sync::mpsc::channel(16)
}

#[tokio::test]
async fn normal_mode_sends_no_notice() {
    let (tx, mut rx) = events_channel().await;
    send_mode_notice("s", &policies(CodingMode::Normal), &tx).await;
    assert!(
        rx.try_recv().is_err(),
        "normal mode has nothing to announce"
    );
}

#[tokio::test]
async fn plan_mode_announces_the_write_limit() {
    let (tx, mut rx) = events_channel().await;
    send_mode_notice("s", &policies(CodingMode::Plan), &tx).await;
    let event = rx.try_recv().expect("plan mode must announce itself");
    match event.kind {
        SessionEventKind::Progress { message } => {
            assert!(message.contains("plan mode"), "{message}");
            assert!(
                message.contains(liberado_coder_core::PLAN_ARTIFACT_REL),
                "the write boundary must be named: {message}"
            );
            assert!(message.contains("shell disabled"), "{message}");
        }
        other => panic!("expected Progress, got {other:?}"),
    }
}

#[tokio::test]
async fn explore_mode_announces_read_only() {
    let (tx, mut rx) = events_channel().await;
    send_mode_notice("s", &policies(CodingMode::Explore), &tx).await;
    let event = rx.try_recv().expect("explore mode must announce itself");
    match event.kind {
        SessionEventKind::Progress { message } => {
            assert!(message.contains("read-only"), "{message}");
        }
        other => panic!("expected Progress, got {other:?}"),
    }
}

struct Transcript {
    store: Arc<GoalSessionStore>,
    grant: liberado_session::SessionGrant,
}

impl Transcript {
    async fn open() -> Self {
        let store = Arc::new(GoalSessionStore::new());
        let mut spec = goal_with_max_turns(0);
        spec.id = Some("s1".into());
        SessionRecordStore::insert(store.as_ref(), GoalSessionRecord::new(spec)).await;
        Self {
            store,
            grant: liberado_session::SessionGrant::default(),
        }
    }
    fn ctx(&self) -> PackContext<'_> {
        PackContext::new(&self.grant, self.store.clone(), "s1")
    }
}

type CancelRx = tokio::sync::watch::Receiver<bool>;

/// Scripted human answers plus a live cancel sender. Both ends must stay alive
/// for the duration of the ask: a dropped input sender reads as `Closed`
/// (cancelled), and a dropped cancel sender would close the watch channel.
async fn scripted_inputs(
    answers: &[&str],
) -> (InputChannel, CancelRx, tokio::sync::watch::Sender<bool>) {
    let (in_tx, in_rx) = tokio::sync::mpsc::channel::<HumanInput>(8);
    for a in answers {
        in_tx.send(HumanInput::new(*a)).await.unwrap();
    }
    drop(in_tx); // closing after the scripted answers makes further asks fail fast
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    (InputChannel::new(in_rx, None), cancel_rx, cancel_tx)
}

#[tokio::test]
async fn each_stop_word_aborts_guidance() {
    for word in ["abort", "stop", "cancel", "ABORT"] {
        let transcript = Transcript::open().await;
        let ctx = transcript.ctx();
        let (events, _rx) = events_channel().await;
        let (mut inputs, mut cancel, _keep_cancel_tx) = scripted_inputs(&[word]).await;
        let pack = pack();
        let answer = pack
            .ask_for_guidance("s", &ctx, &events, &mut inputs, &mut cancel, "it failed")
            .await
            .expect("answer delivered");
        assert!(
            matches!(answer, HumanAnswer::Aborted),
            "`{word}` must abort"
        );
    }
}

#[tokio::test]
async fn other_answers_carry_as_guidance() {
    let transcript = Transcript::open().await;
    let ctx = transcript.ctx();
    let (events, _rx) = events_channel().await;
    let (mut inputs, mut cancel, _keep_cancel_tx) =
        scripted_inputs(&["try the config struct instead"]).await;
    let pack = pack();
    let answer = pack
        .ask_for_guidance("s", &ctx, &events, &mut inputs, &mut cancel, "it failed")
        .await
        .expect("answer delivered");
    match answer {
        HumanAnswer::Guidance(text) => {
            assert_eq!(text, "try the config struct instead");
        }
        HumanAnswer::NoAnswer | HumanAnswer::Aborted => {
            panic!("expected Guidance")
        }
    }
}

#[tokio::test]
async fn a_closed_input_channel_ends_the_session_cancelled() {
    let transcript = Transcript::open().await;
    let ctx = transcript.ctx();
    let (events, _rx) = events_channel().await;
    // A `_named` binding would keep the sender alive; scope it so the channel closes.
    let mut inputs = {
        let (in_tx, in_rx) = tokio::sync::mpsc::channel::<HumanInput>(1);
        drop(in_tx);
        InputChannel::new(in_rx, None)
    };
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let mut cancel = cancel_rx;
    let pack = pack();
    let result = pack
        .ask_for_guidance("s", &ctx, &events, &mut inputs, &mut cancel, "it failed")
        .await;
    assert!(matches!(result, Err(PackError::Cancelled)));
}

#[tokio::test]
async fn a_fanout_child_may_not_nest_subtasks() {
    let pack = pack();
    let payload_json = serde_json::json!({ "fanout_child": true, "subtasks": [ {"label": "a", "description": "do a"} ] });
    let payload = CodingGoalPayload::parse(&payload_json).unwrap();
    let transcript = Transcript::open().await;
    let ctx = transcript.ctx();
    let (events, _rx) = events_channel().await;
    let result = pack
        .maybe_run_fanout(
            "s",
            &goal_with_max_turns(0),
            &payload,
            &payload_json,
            &ctx,
            Path::new("/tmp"),
            "model",
            &events,
        )
        .await;
    assert!(
        matches!(&result, Err(PackError::Setup(msg)) if msg.contains("cannot nest")),
        "nested fan-out must be refused: {result:?}"
    );
}

#[tokio::test]
async fn a_child_without_subtasks_defers_to_single_agent_build() {
    let pack = pack();
    let payload_json = serde_json::json!({ "fanout_child": true });
    let payload = CodingGoalPayload::parse(&payload_json).unwrap();
    let transcript = Transcript::open().await;
    let ctx = transcript.ctx();
    let (events, _rx) = events_channel().await;
    let result = pack
        .maybe_run_fanout(
            "s",
            &goal_with_max_turns(0),
            &payload,
            &payload_json,
            &ctx,
            Path::new("/tmp"),
            "model",
            &events,
        )
        .await
        .expect("no subtasks is not an error");
    assert!(result.is_none(), "{result:?}");
}

fn frozen_contract() -> liberado_coder_core::GoalContract {
    use liberado_coder_core::{FreezeAuthority, GoalContractDraft};
    let draft = GoalContractDraft {
        description: "add a --version flag".into(),
        success_criteria: vec!["prints a semver".into()],
        verifiers: Vec::new(),
        out_of_scope: Vec::new(),
        assumed_defaults: Vec::new(),
        domain_hint: None,
        verify_profile: None,
    };
    liberado_coder_core::GoalContract::freeze("c1", draft, FreezeAuthority::Human)
        .expect("draft freezes")
}

#[tokio::test]
async fn a_tampered_contract_is_refused_before_building() {
    let mut contract = frozen_contract();
    contract.content_hash = "deadbeef".into();
    let mut request: CoderRunRequest = serde_json::from_value(serde_json::json!({
        "task": {"id": "t", "description": "d"},
        "workspace": {"root": "/tmp/ws", "base_ref": "main"},
        "config": {
            "backend": "loop",
            "planner": {"model": "p"},
            "coder": {"model": "c"},
            "critic": {"model": "cr"}
        }
    }))
    .unwrap();
    let (events, _rx) = events_channel().await;
    let result = apply_contract_if_any(&Some(Box::new(contract)), &mut request, "s", &events).await;
    assert!(
        matches!(&result, Err(PackError::Setup(msg)) if msg.contains("refusing to build")),
        "{result:?}"
    );
}

/// A backend whose child "does work" by committing a file on its branch, so the
/// fan-out pipeline gets real branch tips and clean merges end to end.
struct CommittingBackend {
    succeed: bool,
}

#[async_trait::async_trait]
impl CoderBackend for CommittingBackend {
    fn name(&self) -> &str {
        "committing"
    }
    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        let root = request.workspace.root.clone();
        std::fs::write(format!("{root}/child-file.txt"), "work\n").unwrap();
        for args in [
            vec!["add", "-A"],
            vec![
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "child work",
            ],
        ] {
            let out = liberado_common::process::std_command("git")
                .args(["-C", &root])
                .args(&args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(CoderRunResult {
            backend: "committing".into(),
            outcome: if self.succeed {
                Outcome::Succeeded
            } else {
                Outcome::Failed
            },
            summary: "child done".into(),
            files_changed: vec!["child-file.txt".into()],
            file_changes: Vec::new(),
            validation_notes: None,
            critic_verdict: None,
            gate_votes: Vec::new(),
            trace_path: None,
            diff_findings: Vec::new(),
            session_findings: Vec::new(),
            remediation: None,
            diagnostics: serde_json::json!({}),
        })
    }
}

fn committing_pack(succeed: bool) -> CodingSessionPack {
    CodingSessionPack::with_backend(
        Arc::new(CommittingBackend { succeed }),
        Arc::new(liberado_provider::MockProvider::new("mock")),
        std::env::temp_dir(),
    )
}

/// A git repo with one commit, so worktrees can branch off HEAD.
async fn parent_repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "root",
            "--allow-empty",
        ],
    ] {
        let out = liberado_common::process::std_command("git")
            .args(["-C", dir.path().to_str().unwrap()])
            .args(&args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let path = dir.path().to_path_buf();
    (dir, path)
}

async fn drive_fanout(
    pack: &CodingSessionPack,
    workspace: &Path,
    payload_json: serde_json::Value,
) -> Result<Option<(GoalResult, Option<bool>)>, PackError> {
    let payload = CodingGoalPayload::parse(&payload_json).unwrap();
    // The goal carries the SAME payload the pack decoded: mode/preflight policy
    // reads ride on `goal.payload`, not on the decoded copy.
    let mut goal = goal_with_max_turns(0);
    goal.payload = payload_json.clone();
    let transcript = Transcript::open().await;
    let ctx = transcript.ctx();
    let (events, mut rx) = events_channel().await;
    let result = pack
        .maybe_run_fanout(
            "s",
            &goal,
            &payload,
            &payload_json,
            &ctx,
            workspace,
            "model",
            &events,
        )
        .await;
    // The FIRST `ValidationFinished` mirrors the fanout REPORT; a ship
    // preflight that then runs emits its own later. Keep the first.
    let mut validation_ok = None;
    while let Ok(event) = rx.try_recv() {
        if let SessionEventKind::ValidationFinished { ok, .. } = event.kind
            && validation_ok.is_none()
        {
            validation_ok = Some(ok);
        }
    }
    result.map(|r| r.map(|g| (g, validation_ok)))
}

/// Without a git repo the children cannot get worktrees; the report still lands
/// with every child failed and the terminal kind honest about it.
#[tokio::test]
async fn unworktreeable_children_fail_the_fanout_honestly() {
    let _env = crate::DATA_DIR_ENV_LOCK.lock().await;
    let data = tempfile::tempdir().unwrap();
    let _restore = RestoreLiberadoDataDir::set_to(data.path());
    let dir = tempfile::tempdir().unwrap();
    let pack = committing_pack(true);
    let (goal_result, validation_ok) = drive_fanout(
        &pack,
        dir.path(),
        serde_json::json!({ "subtasks": [ {"label": "a", "description": "do a"} ] }),
    )
    .await
    .expect("fanout runs")
    .expect("subtasks produce a result");
    assert_eq!(
        validation_ok,
        Some(false),
        "the report failed; the event says so"
    );
    assert_eq!(
        goal_result.terminal,
        TerminalKind::Failed,
        "{goal_result:?}"
    );
}

#[tokio::test]
async fn a_clean_fanout_with_preflight_skipped_succeeds() {
    let _env = crate::DATA_DIR_ENV_LOCK.lock().await;
    let data = tempfile::tempdir().unwrap();
    let _restore = RestoreLiberadoDataDir::set_to(data.path());
    let (_dir, repo) = parent_repo().await;
    let pack = committing_pack(true);
    let (goal_result, validation_ok) = drive_fanout(
        &pack,
        &repo,
        serde_json::json!({
            "skip_preflight": true,
            "subtasks": [ {"label": "a", "description": "do a"} ]
        }),
    )
    .await
    .expect("fanout runs")
    .expect("subtasks produce a result");
    assert_eq!(validation_ok, Some(true));
    assert_eq!(
        goal_result.terminal,
        TerminalKind::Succeeded,
        "{goal_result:?}"
    );
}

/// The ship preflight is the last gate: a green fan-out whose ship bar fails
/// must land `Failed` with the preflight summary, not sail through.
#[tokio::test]
async fn a_failing_ship_preflight_fails_a_green_fanout() {
    let _env = crate::DATA_DIR_ENV_LOCK.lock().await;
    let data = tempfile::tempdir().unwrap();
    let _restore = RestoreLiberadoDataDir::set_to(data.path());
    let (_dir, repo) = parent_repo().await;
    let pack = committing_pack(true);
    let (goal_result, validation_ok) = drive_fanout(
        &pack,
        &repo,
        serde_json::json!({
            "project": "probe",
            "preflight": { "required": true, "profile": "ship" },
            "subtasks": [ {"label": "a", "description": "do a"} ]
        }),
    )
    .await
    .expect("fanout runs")
    .expect("subtasks produce a result");
    assert_eq!(
        validation_ok,
        Some(true),
        "the fanout report itself was green"
    );
    assert_eq!(
        goal_result.terminal,
        TerminalKind::Failed,
        "{goal_result:?}"
    );
    assert!(
        goal_result.summary.contains("preflight"),
        "the operator must see WHY: {goal_result:?}"
    );
}

// ── select_attempt_workspace ────────────────────────────────────────────────

#[tokio::test]
async fn a_fanout_child_runs_in_place_even_on_a_git_repo() {
    let dir = tempfile::tempdir().unwrap();
    let out = liberado_common::process::std_command("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let payload_json = serde_json::json!({ "fanout_child": true });
    let payload = CodingGoalPayload::parse(&payload_json).unwrap();
    let (events, _rx) = events_channel().await;
    let (ws, sandbox) = select_attempt_workspace(
        "s1",
        &payload,
        &policies(CodingMode::Normal),
        dir.path(),
        &events,
    )
    .await
    .expect("workspace selected");
    assert_eq!(
        ws,
        dir.path(),
        "a fanout child works in place, not in a durable worktree"
    );
    assert!(matches!(sandbox, SandboxSpec::HostLocal));
}

#[tokio::test]
async fn explore_mode_runs_in_place_even_on_a_git_repo() {
    let dir = tempfile::tempdir().unwrap();
    let out = liberado_common::process::std_command("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let payload = CodingGoalPayload::parse(&serde_json::json!({})).unwrap();
    let (events, _rx) = events_channel().await;
    let (ws, sandbox) = select_attempt_workspace(
        "s1",
        &payload,
        &policies(CodingMode::Explore),
        dir.path(),
        &events,
    )
    .await
    .expect("workspace selected");
    assert_eq!(ws, dir.path(), "exploration must not spin up worktrees");
    assert!(matches!(sandbox, SandboxSpec::HostLocal));
}

/// An explicit `force_host_local` payload keeps even a git-repo build mode in
/// place instead of a durable worktree.
#[tokio::test]
async fn forced_host_local_runs_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let out = liberado_common::process::std_command("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let payload =
        CodingGoalPayload::parse(&serde_json::json!({ "force_host_local": true })).unwrap();
    let (events, _rx) = events_channel().await;
    let (ws, sandbox) = select_attempt_workspace(
        "s1",
        &payload,
        &policies(CodingMode::Normal),
        dir.path(),
        &events,
    )
    .await
    .expect("workspace selected");
    assert_eq!(ws, dir.path(), "the operator's host-local override wins");
    assert!(matches!(sandbox, SandboxSpec::HostLocal));
}

#[test]
fn force_host_local_defaults_to_false_and_reads_true() {
    assert!(
        !CodingGoalPayload::parse(&serde_json::json!({}))
            .unwrap()
            .force_host_local()
    );
    assert!(
        CodingGoalPayload::parse(&serde_json::json!({ "force_host_local": true }))
            .unwrap()
            .force_host_local()
    );
}
