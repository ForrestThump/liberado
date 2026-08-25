//! Entry-path and fan-out tests for `CodingSessionPack::run` — split from `tests.rs`
//! (module-health: a fresh file carries its own metrics).

use super::tests::{CLARIFY_JSON, RestoreLiberadoDataDir, ScriptedBackend, goal};
use super::*;
use crate::DATA_DIR_ENV_LOCK;
use liberado_provider::{CompletionResponse, MockProvider};
use liberado_session::HumanInput;
use tokio::sync::mpsc;

// ── run(): the resume entry branches ─────────────────────────────────────────────

#[tokio::test]
async fn a_cancelled_session_refuses_to_start() {
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::new(provider, std::env::temp_dir());
    let (ev_tx, _ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    let inputs = InputChannel::new(in_rx, None);
    let (c_tx, cancel) = tokio::sync::watch::channel(true);

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = goal("make a todo cli");
    spec.id = Some("cx".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant::default();
    let ctx = PackContext::new(&grant, store.clone(), "cx");

    drop(c_tx); // keep the watch alive but preset to cancelled
    let err = pack
        .run("cx", &goal("x"), &ctx, ev_tx, inputs, cancel)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PackError::Cancelled),
        "a pre-cancelled session must not start: {err:?}"
    );
}

#[tokio::test]
async fn an_unparseable_payload_fails_setup_before_any_path_is_touched() {
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::new(provider, std::env::temp_dir());
    let (ev_tx, _ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    let inputs = InputChannel::new(in_rx, None);
    let (c_tx, cancel) = tokio::sync::watch::channel(false);

    let mut g = goal("make a todo cli");
    g.payload = serde_json::json!("not an object");
    drop(c_tx);

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some("badpayload".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant::default();
    let ctx = PackContext::new(&grant, store.clone(), "badpayload");

    let err = pack
        .run("badpayload", &g, &ctx, ev_tx, inputs, cancel)
        .await
        .unwrap_err();
    match err {
        PackError::Setup(msg) => assert!(
            msg.contains("invalid coding goal payload"),
            "setup error should name the payload: {msg}"
        ),
        other => panic!("expected Setup, got {other:?}"),
    }
}

#[tokio::test]
async fn mid_build_resume_without_a_checkpoint_skips_intake_and_rebuilds() {
    // Prior events say the build already started. A fresh run with this provider script and a
    // closed human channel could only end in intake limbo — so reaching Succeeded proves the
    // resume path skipped intake and went straight to the build phase.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backend = Arc::new(ScriptedBackend {
        seen: seen.clone(),
        fail_attempts: 0,
    });
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        vec![CompletionResponse::text(CLARIFY_JSON)],
    ));
    let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

    // An explicit workspace root keeps the build phase off the durable-worktree path, whose
    // base directory comes from the process-global LIBERADO_DATA_DIR other tests set.
    let workspace = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    // The build phase resolves its attempt workspace under LIBERADO_DATA_DIR; pin it here so a
    // concurrent test mid-set cannot redirect this run into someone else's tree.
    let _env = DATA_DIR_ENV_LOCK.lock().await;
    let _restore_env = RestoreLiberadoDataDir::set_to(data.path());

    let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    drop(_in_tx); // nobody is home
    let inputs = InputChannel::new(in_rx, None);
    let (c_tx, cancel) = tokio::sync::watch::channel(false);
    drop(c_tx);

    // Unique per run: a fixed id would collide with durable-worktree state left by an
    // earlier crashed run under std::env::temp_dir().
    let sid = format!("resume-nock-{}", std::process::id());
    let sid = sid.as_str();
    let mut g = goal("make a todo cli");
    g.payload = serde_json::json!({
        "workspace_root": workspace.path().to_string_lossy(),
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some(sid.into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    liberado_session::SessionRecordStore::push_event(
        store.as_ref(),
        SessionEvent::new(
            sid,
            SessionEventKind::RoleStarted {
                role: "coder".into(),
                model: "m".into(),
            },
        ),
    )
    .await;

    let grant = liberado_session::SessionGrant::default();
    let ctx = PackContext::new(&grant, store.clone(), sid);

    let out = pack
        .run(sid, &g, &ctx, ev_tx, inputs, cancel)
        .await
        .expect("resume without a checkpoint rebuilds directly");
    assert_eq!(out.terminal, TerminalKind::Succeeded, "{out:?}");
    assert_eq!(seen.lock().unwrap().len(), 1, "the backend ran once");

    let mut events = Vec::new();
    while let Ok(e) = ev_rx.try_recv() {
        events.push(e);
    }
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::Progress { message } if message.contains("intake skipped")
        )),
        "resume skips intake entirely, so no skip announcement is expected"
    );
}

#[tokio::test]
async fn mid_build_resume_with_a_checkpoint_but_no_workspace_fails_the_shadow_git_open() {
    // The checkpoint exists, but the workspace it names does not. open_or_init must fail before
    // anything is restored, and the failure must say why.
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::new(provider, std::env::temp_dir());

    let (ev_tx, _ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    let inputs = InputChannel::new(in_rx, None);
    let (c_tx, cancel) = tokio::sync::watch::channel(false);
    drop(c_tx);

    let sid = "res-missing-ws";
    let mut g = goal("make a todo cli");
    g.payload = serde_json::json!({
        "workspace_root": "/nonexistent/liberado-resume-test-workspace",
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some(sid.into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    liberado_session::SessionRecordStore::push_event(
        store.as_ref(),
        SessionEvent::new(
            sid,
            SessionEventKind::RoleStarted {
                role: "coder".into(),
                model: "m".into(),
            },
        ),
    )
    .await;
    liberado_session::SessionRecordStore::push_event(
        store.as_ref(),
        SessionEvent::new(
            sid,
            SessionEventKind::Checkpoint {
                id: "abc123".into(),
                label: "attempt-0-post".into(),
                tree_hash: "tree1".into(),
            },
        ),
    )
    .await;

    let grant = liberado_session::SessionGrant::default();
    let ctx = PackContext::new(&grant, store.clone(), sid);

    let err = pack
        .run(sid, &g, &ctx, ev_tx, inputs, cancel)
        .await
        .unwrap_err();
    match err {
        PackError::Failed(msg) => assert!(
            msg.contains("mid-build resume: open shadow-git failed"),
            "the open failure must be named: {msg}"
        ),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn mid_build_resume_restores_the_last_checkpoint_into_the_workspace() {
    use liberado_coder_sandbox::ShadowGit;

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backend = Arc::new(ScriptedBackend {
        seen: seen.clone(),
        fail_attempts: 0,
    });
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

    let workspace = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let _env = DATA_DIR_ENV_LOCK.lock().await;
    let _restore_env = RestoreLiberadoDataDir::set_to(data.path());

    let sid = "res-restore-ok";
    // Snapshot the real file, then dirty the work-tree with a marker that post-dates the
    // snapshot. A successful restore removes the marker; that is the observable.
    std::fs::write(workspace.path().join("kept.txt"), "v1").unwrap();
    let sg = ShadowGit::open_or_init(workspace.path(), sid).unwrap();
    let cp = sg.snapshot("pre-park").await.unwrap();
    std::fs::write(workspace.path().join("marker.txt"), "post-snapshot").unwrap();

    let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    let inputs = InputChannel::new(in_rx, None);
    let (c_tx, cancel) = tokio::sync::watch::channel(false);
    drop(c_tx);

    let mut g = goal("make a todo cli");
    g.payload = serde_json::json!({
        "workspace_root": workspace.path().to_string_lossy(),
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some(sid.into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    liberado_session::SessionRecordStore::push_event(
        store.as_ref(),
        SessionEvent::new(
            sid,
            SessionEventKind::RoleStarted {
                role: "coder".into(),
                model: "m".into(),
            },
        ),
    )
    .await;
    liberado_session::SessionRecordStore::push_event(
        store.as_ref(),
        SessionEvent::new(
            sid,
            SessionEventKind::Checkpoint {
                id: cp.id.clone(),
                label: cp.label.clone(),
                tree_hash: cp.tree_hash.clone(),
            },
        ),
    )
    .await;

    let grant = liberado_session::SessionGrant::default();
    let ctx = PackContext::new(&grant, store.clone(), sid);

    let out = pack
        .run(sid, &g, &ctx, ev_tx, inputs, cancel)
        .await
        .expect("restore succeeds and the build phase runs");
    assert_eq!(out.terminal, TerminalKind::Succeeded, "{out:?}");
    assert_eq!(seen.lock().unwrap().len(), 1, "the backend ran once");

    assert!(
        !workspace.path().join("marker.txt").exists(),
        "restore must roll the work-tree back to the snapshot"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("kept.txt")).unwrap(),
        "v1",
        "snapshot content survives the restore"
    );

    let mut events = Vec::new();
    while let Ok(e) = ev_rx.try_recv() {
        events.push(e);
    }
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::Progress { message }
                if message.contains(&format!("restored checkpoint {} ({})", cp.label, cp.id))
        )),
        "the restore is narrated with its label and id: {events:?}"
    );
}

// ── run(): the fan-out branch of maybe_run_fanout ────────────────────────────────

fn two_subtasks() -> serde_json::Value {
    serde_json::json!([
        { "label": "alpha", "description": "add alpha", "success_criteria": ["alpha done"] },
        { "label": "beta", "description": "add beta" }
    ])
}

async fn run_fanout_goal(
    sid: &str,
    extra_payload: serde_json::Value,
    fail_attempts: u32,
    overrides: serde_json::Value,
) -> (
    Result<GoalResult, PackError>,
    Vec<SessionEvent>,
    usize,
    tempfile::TempDir,
) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backend = Arc::new(ScriptedBackend {
        seen: seen.clone(),
        fail_attempts,
    });
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

    let workspace = tempfile::tempdir().unwrap();
    let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(256);
    let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    let inputs = InputChannel::new(in_rx, None);
    let (c_tx, cancel) = tokio::sync::watch::channel(false);
    drop(c_tx);

    let mut payload = serde_json::json!({
        "workspace_root": workspace.path().to_string_lossy(),
        "intake": { "enabled": false },
        "force_host_local": true,
    });
    for (k, v) in extra_payload.as_object().unwrap() {
        payload[k] = v.clone();
    }
    let mut g = goal("parallel work");
    g.payload = payload;

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some(sid.into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant {
        overrides,
        ..Default::default()
    };
    let ctx = PackContext::new(&grant, store.clone(), sid);

    let out = pack.run(sid, &g, &ctx, ev_tx, inputs, cancel).await;
    let mut events = Vec::new();
    while let Ok(e) = ev_rx.try_recv() {
        events.push(e);
    }
    let calls = seen.lock().unwrap().len();
    (out, events, calls, workspace)
}

#[tokio::test]
async fn a_fanout_child_cannot_nest_its_own_subtasks() {
    let sid = "fanout-nested";
    let (out, _events, _calls, _ws) = run_fanout_goal(
        sid,
        serde_json::json!({ "fanout_child": true, "subtasks": two_subtasks() }),
        0,
        serde_json::json!({}),
    )
    .await;
    match out.unwrap_err() {
        PackError::Setup(msg) => assert!(
            msg.contains("cannot nest further subtasks"),
            "the nesting refusal must be named: {msg}"
        ),
        other => panic!("expected Setup, got {other:?}"),
    }
}

#[tokio::test]
async fn fanout_runs_children_in_process_and_reports_their_files() {
    let sid = "fanout-ok";
    let (out, events, calls, _ws) = run_fanout_goal(
        sid,
        serde_json::json!({
            "subtasks": two_subtasks(),
        }),
        0,
        // Serialised via grant override: concurrent `git worktree add` in
        // one parent repo races the index lock on loaded runners (see
        // failed_fanout_children_end_the_goal_failed). Which pool size is
        // announced is covered by the override-ceiling test.
        serde_json::json!({ "max_concurrent_coding_subagents": 1 }),
    )
    .await;
    let out = out.expect("in-process fan-out reaches a terminal result");
    assert_eq!(out.terminal, TerminalKind::Succeeded, "{out:?}");
    assert_eq!(calls, 2, "one backend call per subtask");
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::Progress { message }
                if message.contains("coding fan-out: 2 subtask(s)")
                    && message.contains("max_concurrent=1")
                    && message.contains("mode=in-process")
        )),
        "the fan-out is announced with its budget: {events:?}"
    );
    assert!(events.iter().any(
        |e| matches!(&e.kind, SessionEventKind::RoleStarted { role, .. } if role == "coder-fanout")
    ));
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::ValidationFinished { ok: true, .. }
        )),
        "a green merge reports validation ok"
    );
}

#[tokio::test]
async fn failed_fanout_children_end_the_goal_failed() {
    let sid = "fanout-fail";
    let (out, events, calls, _ws) = run_fanout_goal(
        sid,
        serde_json::json!({ "subtasks": two_subtasks() }),
        u32::MAX, // every child attempt fails
        serde_json::json!({}),
    )
    .await;
    let out = out.expect("a red fan-out is still a terminal result, not an error");
    assert_eq!(out.terminal, TerminalKind::Failed, "{out:?}");
    // Not pinned at 2: when two children race `git worktree add` in the same parent repo,
    // one can lose the index lock and error out before its backend call. The point here is
    // that whatever reached the backend came back red and the goal reports Failed.
    assert!(calls >= 1, "at least one child reached the backend");
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::ValidationFinished { ok: false, .. }
        )),
        "a red merge reports validation failure: {events:?}"
    );
}

#[tokio::test]
async fn fanout_honors_the_override_concurrency_ceiling() {
    let sid = "fanout-override";
    let (_out, events, _calls, _ws) = run_fanout_goal(
        sid,
        serde_json::json!({ "subtasks": two_subtasks() }),
        0,
        // No payload value: the profile override is what bounds the pool.
        serde_json::json!({ "max_concurrent_coding_subagents": 1 }),
    )
    .await;
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::Progress { message } if message.contains("max_concurrent=1")
        )),
        "the override ceiling reaches the announcement: {events:?}"
    );
}

#[tokio::test]
async fn fanout_success_still_faces_the_ship_preflight() {
    let sid = "fanout-preflight";
    let fail = if cfg!(windows) { "exit /B 1" } else { "exit 1" };
    let (out, _events, calls, _ws) = run_fanout_goal(
        sid,
        serde_json::json!({
            "subtasks": two_subtasks(),
            "preflight": {
                "required": true,
                "steps": [{ "name": "must-fail", "run": fail }]
            },
        }),
        0,
        // Serialise the children via grant override: concurrent `git
        // worktree add` in the same parent repo races the index lock (see
        // failed_fanout_children_end_the_goal_failed), and this test needs
        // BOTH branches to merge so a green fan-out reaches its ship bar.
        serde_json::json!({ "max_concurrent_coding_subagents": 1 }),
    )
    .await;
    let out = out.expect("preflight runs after a green fan-out");
    assert_eq!(calls, 2);
    assert_eq!(
        out.terminal,
        TerminalKind::Failed,
        "a red ship bar fails the goal even though every branch merged: {out:?}"
    );
    assert!(out.summary.contains("must-fail"), "{}", out.summary);
    assert!(
        out.diagnostics.get("preflight").is_some(),
        "diagnostics carry the preflight report: {}",
        out.diagnostics
    );
}
