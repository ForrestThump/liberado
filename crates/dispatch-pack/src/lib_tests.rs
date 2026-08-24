//! Split from `lib.rs` for module-health boundaries.

use super::*;
use liberado_common::WriteProvenance;
use liberado_common::{
    Capability, CapabilitySet, Delivery, DispatchAction, DispatchDecision, Outcome, ProposalSigner,
    Report,
};
use liberado_config_loader::DispatchTuning;
use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
use liberado_orchestrator::SubDispatch;
use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
use liberado_session::{
    DomainHint, GoalSessionHub, GoalSessionStore, SessionGrant, SessionStatus, TerminalKind,
};
use std::sync::Arc;

struct NoopFactory;
#[async_trait]
impl RuntimeFactory for NoopFactory {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        Ok(Box::new(NoopRuntime))
    }
}

struct NoopRuntime;
#[async_trait]
impl ToolRuntime for NoopRuntime {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Err("noop".into())
    }
}

fn make_pack(
    decision: DispatchDecision,
    exec_script: Vec<CompletionResponse>,
    pool_caps: CapabilitySet,
) -> DispatchPack {
    let dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        )),
        DispatchTuning::default(),
        4,
    );
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script("exec", exec_script)),
        NoopFactory,
        pool_caps,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        DEFAULT_POOL,
    );
    DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool(DEFAULT_POOL, dispatcher, orchestrator)
}

#[tokio::test]
async fn pack_runs_execute_direct_to_a_terminal_session() {
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "routine work".into(),
    };
    let pack = make_pack(
        decision,
        vec![CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c",
            SUBMIT_REPORT_TOOL,
            serde_json::json!({ "outcome": "succeeded", "summary": "done via pack" }),
        )])],
        CapabilitySet::empty(),
    );

    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let id = hub
        .start_with_grant(
            GoalSpec {
                id: None,
                description: "summarize today".into(),
                success_criteria: vec![],
                domain: DomainHint::from(DISPATCH_DOMAIN),
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({}),
            },
            SessionGrant::default(),
        )
        .await
        .expect("start");

    // Poll until terminal.
    let snap = wait_terminal(&hub, &id).await;
    assert_eq!(snap.session.status, SessionStatus::Succeeded);
    assert_eq!(
        snap.session.result.as_ref().unwrap().summary,
        "done via pack"
    );
    assert_eq!(
        snap.session.result.as_ref().unwrap().terminal,
        TerminalKind::Succeeded
    );
    // Decision narrated as a progress event.
    assert!(
        snap.events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::Progress { message }
                if message.contains("ExecuteDirect") && message.contains("routine work")
        )),
        "decision must be narrated: {:#?}",
        snap.events
    );
}

#[tokio::test]
async fn pack_honours_a_narrow_session_grant_over_a_wide_pool() {
    // Pool would allow tasks-mcp; session grant is empty — ExecuteDirect must not open a runtime
    // for any MCP (same proof as E1, through the pack).
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "would write if allowed".into(),
    };
    let pool_caps = CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]);
    let pack = make_pack(
        decision,
        vec![CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c",
            SUBMIT_REPORT_TOOL,
            serde_json::json!({ "outcome": "succeeded", "summary": "ran with no tools" }),
        )])],
        pool_caps,
    );

    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    // Empty grant — narrower than the pool.
    let id = hub
        .start_with_grant(
            GoalSpec {
                id: None,
                description: "try a write".into(),
                success_criteria: vec![],
                domain: DomainHint::from(DISPATCH_DOMAIN),
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({}),
            },
            SessionGrant {
                capabilities: CapabilitySet::empty(),
                profile: Some("read-only-cron".into()),
                overrides: serde_json::Value::Null,
                ..Default::default()
            },
        )
        .await
        .expect("start");

    let snap = wait_terminal(&hub, &id).await;
    assert_eq!(snap.session.status, SessionStatus::Succeeded);
    // With zero grants the orchestrator uses NoMcpRuntime; the mock still submits a report.
    assert!(
        snap.session
            .result
            .as_ref()
            .unwrap()
            .summary
            .contains("ran with no tools")
    );
}

#[tokio::test]
async fn clarify_fails_honestly() {
    use liberado_common::BlockReason;
    let decision = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["which project?".into()],
            what_blocked: BlockReason::Ambiguous,
        },
        confidence: 0.5,
        rationale: "ambiguous".into(),
    };
    let pack = make_pack(decision, vec![], CapabilitySet::empty());
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);
    let id = hub
        .start_with_grant(
            GoalSpec {
                id: None,
                description: "do the thing".into(),
                success_criteria: vec![],
                domain: DomainHint::from(DISPATCH_DOMAIN),
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({}),
            },
            SessionGrant::default(),
        )
        .await
        .unwrap();
    let snap = wait_terminal(&hub, &id).await;
    assert_eq!(snap.session.status, SessionStatus::Failed);
    assert!(
        snap.session
            .result
            .as_ref()
            .unwrap()
            .summary
            .contains("which project?")
    );
}

async fn wait_terminal(hub: &Arc<GoalSessionHub>, id: &str) -> liberado_session::SessionSnapshot {
    for _ in 0..200 {
        if let Some(snap) = hub.snapshot(id).await
            && snap.session.status.is_terminal()
        {
            return snap;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("session {id} did not reach terminal");
}

#[test]
fn disposition_reported_maps_through_terminal_summary() {
    let d = Disposition::Reported(Report {
        outcome: Outcome::Succeeded,
        summary: "ok".into(),
        artifacts: vec![],
        new_high_signal_facts: vec![],
        follow_up: None,
        deferred_to_human: false,
        repeat_calls: 0,
    });
    let (t, s) = d.terminal_summary();
    assert_eq!(t, TerminalKind::Succeeded);
    assert_eq!(s, "ok");
}

#[test]
fn pool_name_defaults_and_reads_payload() {
    let mut goal = GoalSpec {
        id: None,
        description: "g".into(),
        success_criteria: vec![],
        domain: DomainHint::from(DISPATCH_DOMAIN),
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload: serde_json::json!({}),
    };
    assert_eq!(DispatchPack::pool_name(&goal), DEFAULT_POOL);
    goal.payload = serde_json::json!({ "pool": "research" });
    assert_eq!(DispatchPack::pool_name(&goal), "research");
}

struct MarkerRuntime {
    root: std::path::PathBuf,
    marker_name: String,
}

#[async_trait]
impl ToolRuntime for MarkerRuntime {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        vec![liberado_provider::ToolDef::new(
            "write_marker",
            "Write a marker file into the workspace root.",
            serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
        )]
    }

    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        let path = self.root.join(format!("{}.txt", self.marker_name));
        tokio::fs::write(&path, "isolated worker wrote here")
            .await
            .map_err(|e| format!("write marker: {e}"))?;
        Ok(format!("wrote {}", path.display()))
    }
}

#[derive(Clone, Default)]
struct RootedMarkerFactory {
    scoped_roots: Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
    unscoped_calls: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl RuntimeFactory for RootedMarkerFactory {
    async fn runtime_for(
        &self,
        allowed_mcps: &[String],
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        self.unscoped_calls
            .lock()
            .unwrap()
            .push(allowed_mcps.to_vec());
        Err(RuntimeSetupError(
            "the parallel path must scope runtimes via runtime_for_in".into(),
        ))
    }

    async fn runtime_for_in(
        &self,
        _allowed_mcps: &[String],
        _provenance: WriteProvenance,
        workspace_root: Option<std::path::PathBuf>,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        let root = workspace_root
            .expect("an isolated worker must receive a workspace root from the caller");
        let mut roots = self.scoped_roots.lock().unwrap();
        roots.push(root.clone());
        let index = roots.len();
        Ok(Box::new(MarkerRuntime {
            root,
            marker_name: format!("worker-{index}"),
        }))
    }
}

fn init_parent_repo(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "dispatch-pack-test")
            .env("GIT_AUTHOR_EMAIL", "dispatch-pack-test@local")
            .env("GIT_COMMITTER_NAME", "dispatch-pack-test")
            .env("GIT_COMMITTER_EMAIL", "dispatch-pack-test@local")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "--quiet"]);
    run(&["config", "user.email", "dispatch-pack-test@local"]);
    run(&["config", "user.name", "dispatch-pack-test"]);
    run(&["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("baseline.txt"), "shared baseline\n").unwrap();
    run(&["add", "baseline.txt"]);
    run(&["commit", "--quiet", "-m", "init"]);
}

#[tokio::test]
async fn dispatch_parallel_runs_each_worker_in_its_own_worktree() {
    let parent = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    init_parent_repo(parent.path());
    let ws_a = WorktreeWorkspace::new(
        parent.path(),
        "worker-a",
        base.path(),
        CommandPolicy::default(),
    )
    .await
    .expect("worktree A");
    let ws_b = WorktreeWorkspace::new(
        parent.path(),
        "worker-b",
        base.path(),
        CommandPolicy::default(),
    )
    .await
    .expect("worktree B");
    assert_ne!(ws_a.root(), ws_b.root());
    let root_a = ws_a.root().to_path_buf();
    let root_b = ws_b.root().to_path_buf();

    let factory = RootedMarkerFactory::default();
    let scoped_roots = factory.scoped_roots.clone();
    let unscoped_calls = factory.unscoped_calls.clone();
    let script = [
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            "write_marker",
            serde_json::json!({}),
        )]),
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c1",
            SUBMIT_REPORT_TOOL,
            serde_json::json!({
                "outcome": "succeeded",
                "summary": "worker wrote its isolated marker",
                "artifacts": [],
                "new_high_signal_facts": [],
            }),
        )]),
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c2",
            "write_marker",
            serde_json::json!({}),
        )]),
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c2",
            SUBMIT_REPORT_TOOL,
            serde_json::json!({
                "outcome": "succeeded",
                "summary": "worker wrote its isolated marker",
                "artifacts": [],
                "new_high_signal_facts": [],
            }),
        )]),
    ];
    let provider = Arc::new(MockProvider::with_script("mock", script));
    let orch = Orchestrator::new(
        provider,
        factory,
        CapabilitySet::from_iter([Capability::ExecuteTool("write_marker".into())]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        DEFAULT_POOL,
    );
    let report = orch
        .dispatch_parallel(
            vec![
                SubDispatch {
                    goal: "write your marker in your own tree".into(),
                    allowed_mcps: vec![],
                    success_criteria: vec![],
                    correlation_id: "corr-a".into(),
                    label: "A".into(),
                    workspace_root: Some(root_a.clone()),
                },
                SubDispatch {
                    goal: "write your marker in your own tree".into(),
                    allowed_mcps: vec![],
                    success_criteria: vec![],
                    correlation_id: "corr-b".into(),
                    label: "B".into(),
                    workspace_root: Some(root_b.clone()),
                },
            ],
            1,
        )
        .await
        .expect("dispatch_parallel with isolated workers");
    assert_eq!(report.outcome, Outcome::Succeeded);
    assert!(report.summary.contains("worker wrote its isolated marker"));
    assert!(unscoped_calls.lock().unwrap().is_empty());
    let scoped = scoped_roots.lock().unwrap();
    assert_eq!(scoped.len(), 2);
    assert!(scoped.contains(&root_a));
    assert!(scoped.contains(&root_b));
    assert!(root_a.join("worker-1.txt").exists());
    assert!(root_b.join("worker-2.txt").exists());
    assert!(!root_a.join("worker-2.txt").exists());
    assert!(!root_b.join("worker-1.txt").exists());
}

#[tokio::test]
async fn pack_refuses_nested_parallel_goals() {
    let pack = make_pack(
        DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: Vec::new(),
                delivery: Delivery::Summarize,
            },
            confidence: 1.0,
            rationale: "unused".into(),
        },
        vec![],
        CapabilitySet::empty(),
    );
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);
    let id = hub
        .start_with_grant(
            GoalSpec {
                id: None,
                description: "nested".into(),
                success_criteria: vec![],
                domain: DomainHint::from(DISPATCH_DOMAIN),
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({
                    "parallel_child": true,
                    "parallel_goals": [{"goal": "a", "label": "A"}],
                }),
            },
            SessionGrant::default(),
        )
        .await
        .expect("start");
    let snap = wait_terminal(&hub, &id).await;
    assert!(
        matches!(snap.session.status, SessionStatus::Failed),
        "nested fan-out must fail: {:?}",
        snap.session.status
    );
}

// ── parallel_route / budget / write_proposal survivors ────────────────────

#[test]
fn parallel_route_rejects_nesting_and_routes_only_real_fan_outs() {
    let base = |payload: serde_json::Value| GoalSpec {
        id: None,
        description: String::new(),
        success_criteria: vec![],
        domain: DomainHint::from(DISPATCH_DOMAIN),
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload,
    };

    // A child may not nest any fan-out — presence alone rejects, even empty.
    let err = parallel_route(&base(serde_json::json!({
        "parallel_child": true,
        "parallel_goals": [{ "goal": "a" }],
    })))
    .expect_err("child with goals must be a nesting error");
    assert!(err.to_string().contains("cannot nest"), "{err}");

    let err = parallel_route(&base(serde_json::json!({
        "parallel_child": true,
        "parallel_goals": [],
    })))
    .expect_err("an empty fan-out on a child is still nesting");
    assert!(err.to_string().contains("cannot nest"), "{err}");

    // A real fan-out routes with its goals carried through.
    let routed = parallel_route(&base(serde_json::json!({
        "parallel_goals": [{ "goal": "a" }, { "goal": "b" }],
    })))
    .expect("a top-level fan-out is legal");
    assert_eq!(routed.as_deref().map(|g| g.len()), Some(2));

    // Everything else is the single-goal path.
    assert!(
        parallel_route(&base(serde_json::json!({})))
            .unwrap()
            .is_none()
    );
    assert!(
        parallel_route(&base(serde_json::json!({ "parallel_child": true })))
            .unwrap()
            .is_none(),
        "a child marker alone requests no fan-out"
    );
    assert!(
        parallel_route(&base(serde_json::json!({ "parallel_goals": [] })))
            .unwrap()
            .is_none(),
        "an empty fan-out on a parent falls through to the single-goal path"
    );
}

#[test]
fn zero_max_turns_means_pack_default_positive_propagates() {
    assert_eq!(
        effective_turn_budget(0),
        None,
        "0 is the pack-default sentinel"
    );
    assert_eq!(effective_turn_budget(5), Some(5));
    assert_eq!(effective_turn_budget(u32::MAX), Some(u32::MAX));
}

struct StubNotifier {
    // `Err` carries the NotifyError payload; `Ok` notifies successfully.
    fail_with: Option<String>,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl liberado_notify::Notifier for StubNotifier {
    async fn notify(&self, _message: &str) -> Result<(), liberado_notify::NotifyError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match &self.fail_with {
            Some(e) => Err(liberado_notify::NotifyError(e.clone())),
            None => Ok(()),
        }
    }
}

fn signed(id: &str) -> SignedProposal {
    ProposalSigner::random().sign(liberado_common::Proposal::pending(
        id,
        format!("corr-{id}"),
        "test-agent",
        liberado_common::ProposedAction::ToolCalls(vec![]),
        "needs your review",
    ))
}

#[tokio::test]
async fn write_proposal_persists_the_note_and_reports_the_ping_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        dir.path().to_path_buf(),
    )
    .with_notifier(Arc::new(StubNotifier {
        fail_with: None,
        calls: Default::default(),
    }));
    let proposal = signed("prop-ok");

    let notified = pack.write_proposal(&proposal).await.unwrap();
    assert!(
        notified,
        "a successful ping means deferred_to_human folds true"
    );

    let written = dir
        .path()
        .join(PROPOSALS_DIR)
        .join(format!("{}.md", proposal.as_proposal().id));
    assert!(
        written.exists(),
        "the note must be on disk at {}",
        written.display()
    );
}

#[tokio::test]
async fn write_proposal_still_writes_when_the_notify_fails() {
    let dir = tempfile::tempdir().unwrap();
    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        dir.path().to_path_buf(),
    )
    .with_notifier(Arc::new(StubNotifier {
        fail_with: Some("transport down".into()),
        calls: Default::default(),
    }));

    let proposal = signed("prop-fail");
    let notified = pack.write_proposal(&proposal).await.unwrap();
    assert!(
        !notified,
        "a failed ping must report false so the chat reply is not suppressed"
    );
    assert!(
        dir.path().join(PROPOSALS_DIR).join("prop-fail.md").exists(),
        "the note survives even when the ping fails"
    );
}

#[tokio::test]
async fn write_proposal_without_a_notifier_reports_false_and_writes() {
    let dir = tempfile::tempdir().unwrap();
    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        dir.path().to_path_buf(),
    );

    let proposal = signed("prop-quiet");
    let notified = pack.write_proposal(&proposal).await.unwrap();
    assert!(!notified, "no channel configured: nothing went out-of-band");
    assert!(
        dir.path()
            .join(PROPOSALS_DIR)
            .join("prop-quiet.md")
            .exists(),
        "the note is still the deliverable"
    );
}
