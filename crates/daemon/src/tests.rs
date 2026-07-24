use super::*;
use crate::helpers::*;
use liberado_common::{McpDescriptor, WriteProvenance, event_source};
use liberado_orchestrator::Disposition;
use liberado_session::TerminalKind;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn grant_component_maps_default_pool_to_the_dispatcher_ceiling() {
    // A permission request stamps the owning pool; the default pool's authority ceiling is the
    // "dispatcher" grant (see configure_daemon), so an "everywhere" grant must land there — not
    // on a literal "default" component that grants nothing.
    assert_eq!(grant_component_for_pool(None), "dispatcher");
    assert_eq!(grant_component_for_pool(Some(DEFAULT_POOL)), "dispatcher");
    // A named pool's ceiling is its own name.
    assert_eq!(grant_component_for_pool(Some("research")), "research");
}

#[test]
fn cron_schedule_name_only_matches_cron_sources() {
    assert_eq!(
        cron_schedule_name("cron:daily-planning"),
        Some("daily-planning")
    );
    // Names may themselves contain colons (rfc3339-ish); only the first split matters.
    assert_eq!(
        cron_schedule_name("cron:weekly:review"),
        Some("weekly:review")
    );
    // Non-cron sources must never trigger delivery.
    assert_eq!(
        cron_schedule_name(event_source::TURBOVAULT_SUBSCRIPTION),
        None
    );
    assert_eq!(cron_schedule_name("delegate"), None);
    assert_eq!(cron_schedule_name("cronies:x"), None); // kind must equal "cron", not just prefix
    assert_eq!(cron_schedule_name("cron"), None); // no name
}

#[test]
fn format_cron_delivery_flags_non_success() {
    let ok = format_cron_delivery("daily-planning", "your brief", TerminalKind::Succeeded);
    assert!(ok.contains("daily-planning") && ok.contains("your brief"));
    assert!(
        !ok.contains('['),
        "success must not carry a status tag: {ok}"
    );

    for bad in [
        TerminalKind::Failed,
        TerminalKind::Cancelled,
        TerminalKind::BudgetExhausted,
    ] {
        let msg = format_cron_delivery("daily-planning", "partial", bad);
        assert!(
            msg.contains(&format!("[{bad:?}]")),
            "non-success must be tagged so it isn't mistaken for a real report: {msg}"
        );
    }
}

async fn temp_daemon() -> (Daemon, TempDir) {
    let dir = TempDir::new().unwrap();
    let daemon = Daemon::open("test", dir.path()).await.unwrap();
    (daemon, dir)
}

#[tokio::test]
async fn external_change_produces_reaction() {
    let (daemon, dir) = temp_daemon().await;
    // A human writes a note directly (not through the adapter) — no matching audit entry.
    std::fs::write(dir.path().join("note.md"), "a human wrote this").unwrap();

    let event = daemon
        .process_change(Path::new("note.md"))
        .await
        .unwrap()
        .expect("external change should produce a reaction");
    assert_eq!(event.event_type, VAULT_NOTE_CHANGED);
    assert_eq!(event.source, event_source::TURBOVAULT_SUBSCRIPTION);
    assert_eq!(event.payload.path.as_deref(), Some("note.md"));
    assert!(event.is_reactable());
}

#[tokio::test]
async fn our_own_write_is_suppressed() {
    let (daemon, _dir) = temp_daemon().await;
    let prov = WriteProvenance::agent("tasks-mcp", "c1");
    daemon
        .vault()
        .write("tasks/today.md", "- [ ] x", None, &prov)
        .await
        .unwrap();

    assert!(
        daemon
            .process_change(Path::new("tasks/today.md"))
            .await
            .unwrap()
            .is_none(),
        "agent write must not trigger a reaction"
    );
}

#[tokio::test]
async fn missing_path_is_suppressed() {
    let (daemon, _dir) = temp_daemon().await;
    assert!(
        daemon
            .process_change(Path::new("nope.md"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn watcher_coalesces_burst_into_single_reaction() {
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon.with_debounce(Duration::from_millis(80));
    let vault_dir = dir.path().to_path_buf();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    // Give the watcher a moment to establish before writing.
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(vault_dir.join("captured.md"), "dropped in from Obsidian").unwrap();

    // Exactly one reaction, despite notify firing Create + Modify + ... for one write.
    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");
    assert_eq!(reaction.event.payload.path.as_deref(), Some("captured.md"));
    assert!(
        matches!(reaction.outcome, ReactionOutcome::Observed),
        "watch-only: no dispatcher attached"
    );

    // No duplicate arrives within a generous margin past the debounce window.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        rx.try_recv().is_err(),
        "the notify burst should have coalesced into a single reaction"
    );

    handle.abort();
}

#[tokio::test]
async fn daemon_routes_reaction_through_dispatcher() {
    use liberado_common::{BlockReason, DispatchAction, DispatchDecision};
    use liberado_config_loader::DispatchTuning;
    use liberado_provider::{CompletionResponse, MockProvider};
    use std::sync::Arc;

    let (daemon, dir) = temp_daemon().await;

    // A dispatcher whose (mock) classifier returns a Clarify — the safe outcome with no MCPs.
    let canned = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["how should I handle this note?".into()],
            what_blocked: BlockReason::Ambiguous,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    let mock = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text(
            serde_json::to_string(&canned).unwrap(),
        )],
    ));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_dispatcher(
            dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        );

    let vault_dir = dir.path().to_path_buf();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(vault_dir.join("idea.md"), "a captured thought").unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    assert_eq!(reaction.event.payload.path.as_deref(), Some("idea.md"));
    // Dispatcher attached, but no orchestrator → decided, not acted.
    let ReactionOutcome::Decided(decision) = reaction.outcome else {
        panic!("expected Decided, got {}", reaction.outcome.label());
    };
    assert!(matches!(decision.action, DispatchAction::Clarify { .. }));

    handle.abort();
}

#[tokio::test]
async fn cron_and_vault_watch_are_interchangeable_event_sources() {
    // The literal proof of Decision 18 checkpoint #3: a cron firing and a real vault change
    // both flow through the exact same `react()` path, over the exact same `Reaction` channel,
    // indistinguishable to the dispatcher except by `event.source`.
    use liberado_common::{BlockReason, DispatchAction, DispatchDecision};
    use liberado_config_loader::DispatchTuning;
    use liberado_cron::{CronEventSource, Schedule};
    use liberado_provider::{CompletionResponse, MockProvider};
    use std::collections::HashSet;
    use std::sync::Arc;

    let (daemon, dir) = temp_daemon().await;

    // Every reaction resolves to the same safe Clarify outcome — two reactions expected (one
    // per source), so the mock needs two scripted responses.
    let canned = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["how should I handle this?".into()],
            what_blocked: BlockReason::Ambiguous,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    let canned_json = serde_json::to_string(&canned).unwrap();
    let mock = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text(canned_json.clone()),
            CompletionResponse::text(canned_json),
        ],
    ));
    let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

    let cron_source = CronEventSource::new(vec![Schedule {
        name: "every-second".into(),
        cron_expr: "* * * * * * *".into(),
        goal: "a cron-dispatched goal".into(),
        pool: None,
        profile: None,
    }])
    .unwrap();

    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_dispatcher(
            dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_cron_source(Box::new(cron_source));

    let vault_dir = dir.path().to_path_buf();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(vault_dir.join("idea.md"), "a captured thought").unwrap();

    let mut sources = HashSet::new();
    for _ in 0..2 {
        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for a reaction")
            .expect("reaction channel closed");
        assert!(matches!(reaction.outcome, ReactionOutcome::Decided(_)));
        sources.insert(reaction.event.source);
    }

    assert!(sources.contains(event_source::TURBOVAULT_SUBSCRIPTION));
    assert!(sources.contains("cron:every-second"));

    handle.abort();
}

#[tokio::test]
async fn a_cron_firing_is_recorded_as_a_background_session_instead_of_vanishing() {
    // S5′ step 5, the whole point: before this, a cron fired, a model was called, the vault was
    // possibly written to — and the only trace was a log line. Nothing to see, nothing to join,
    // nothing to review. Now the reaction *is* a session, in the same store as your chats.
    use liberado_common::{DispatchAction, DispatchDecision};
    use liberado_config_loader::DispatchTuning;
    use liberado_cron::{CronEventSource, Schedule};
    use liberado_dispatch_pack::{DISPATCH_DOMAIN, DispatchPack};
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
    use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
    use liberado_session::{
        GoalSessionHub, GoalSessionStore, SessionEventKind, SessionStatus, TerminalKind, Visibility,
    };
    use std::sync::Arc;

    struct NoopRuntime;
    #[async_trait::async_trait]
    impl ToolRuntime for NoopRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    struct NoopFactory;
    #[async_trait::async_trait]
    impl RuntimeFactory for NoopFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(NoopRuntime))
        }
    }

    let (daemon, _dir) = temp_daemon().await;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.95,
        rationale: "a nightly summary is routine".into(),
    };
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
        Arc::new(MockProvider::with_script(
            "exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "summarized today" }),
            )])],
        )),
        NoopFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "default",
    );

    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool("default", dispatcher, orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let cron = CronEventSource::new(vec![Schedule {
        name: "nightly".into(),
        cron_expr: "* * * * * * *".into(), // every second, so the test doesn't wait
        goal: "summarize today's decisions".into(),
        pool: None,
        profile: None,
    }])
    .unwrap();

    // Hub path only needs a dispatcher context for the pool name / grant; the pack owns the
    // actual classify+execute engines. Attach a dummy pool dispatcher so pool lookup succeeds.
    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_cron_source(Box::new(cron))
        .with_goal_hub(hub.clone());

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for the cron reaction")
        .expect("reaction channel closed");
    assert_eq!(reaction.event.source, "cron:nightly");
    let session_id = match reaction.outcome {
        ReactionOutcome::Dispatched { session_id } => session_id,
        other => panic!("expected Dispatched, got {}", other.label()),
    };
    assert_eq!(
        hub.snapshot(&session_id)
            .await
            .unwrap()
            .session
            .goal
            .domain
            .as_str(),
        DISPATCH_DOMAIN
    );

    // Session runs concurrently — wait for terminal.
    let snap = hub.await_terminal(&session_id).await.expect("terminal");
    let row = &snap.session;

    // Nobody was watching — and the record says so, which is what `Visibility` is *for*.
    assert_eq!(row.visibility, Visibility::Background);
    // It says what it was for, not merely that something happened.
    assert_eq!(row.goal.description, "summarize today's decisions");
    // Tied back to the dispatch journal, with no parent conversation — a cron has none.
    let origin = row
        .goal
        .origin
        .as_ref()
        .expect("origin carries correlation");
    assert!(
        origin
            .correlation_id
            .as_deref()
            .unwrap()
            .starts_with("cron:nightly:")
    );
    assert!(origin.conversation_id.is_none());
    // It ran to a real terminal state carrying the executor's own report.
    assert_eq!(row.status, SessionStatus::Succeeded);
    assert_eq!(row.result.as_ref().unwrap().summary, "summarized today");
    assert_eq!(
        row.result.as_ref().unwrap().terminal,
        TerminalKind::Succeeded
    );

    // And it left a readable transcript, not just a status.
    let events = &snap.events;
    assert!(matches!(
        events.first().map(|e| &e.kind),
        Some(SessionEventKind::SessionStarted { .. })
    ));
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::Progress { message }
                if message.contains("ExecuteDirect")
                    && message.contains("a nightly summary is routine")
        )),
        "the dispatch decision and its rationale should be narrated into the transcript: \
             {events:#?}"
    );

    handle.abort();
}

#[tokio::test]
async fn a_reaction_that_needed_a_human_fails_honestly_rather_than_reporting_success() {
    // A background session's status is the only thing a human sees at a glance. A Clarify means
    // the reaction stopped and asked a question nobody was there to answer — so it must not be
    // green. The unanswered questions have to survive into the summary, or they're lost.
    use liberado_common::{BlockReason, DispatchAction, DispatchDecision};
    use liberado_config_loader::DispatchTuning;
    use liberado_dispatch_pack::DispatchPack;
    use liberado_provider::{CompletionResponse, MockProvider};
    use liberado_session::{GoalSessionHub, GoalSessionStore, SessionStatus, Visibility};
    use std::sync::Arc;

    let (daemon, _dir) = temp_daemon().await;

    let canned = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["which project did you mean?".into()],
            what_blocked: BlockReason::Ambiguous,
        },
        confidence: 0.9,
        rationale: "ambiguous".into(),
    };
    let dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&canned).unwrap(),
            )],
        )),
        DispatchTuning::default(),
        4,
    );
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script("exec", Vec::new())),
        NoopFactoryForClarify,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "default",
    );

    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool("default", dispatcher, orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_goal_hub(hub.clone());

    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(200)).await;
    sender
        .send(Event::trigger(
            "WebhookFired",
            "webhook:nightly",
            "webhook:nightly:1",
            liberado_common::EventPayload {
                summary: Some("tidy up the inbox".into()),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed");
    let session_id = match reaction.outcome {
        ReactionOutcome::Dispatched { session_id } => session_id,
        other => panic!("expected Dispatched, got {}", other.label()),
    };
    let snap = hub.await_terminal(&session_id).await.unwrap();
    let row = &snap.session;
    // A webhook is just as unattended as a cron — same seam, same treatment.
    assert_eq!(row.visibility, Visibility::Background);
    assert_eq!(
        row.status,
        SessionStatus::Failed,
        "a reaction that could not proceed without a human did not succeed"
    );
    let summary = &row.result.as_ref().unwrap().summary;
    assert!(
        summary.contains("which project did you mean?"),
        "the unanswered question must survive into the summary, got: {summary}"
    );

    handle.abort();
}

#[tokio::test]
async fn a_reaction_whose_execution_blew_up_says_so_instead_of_blaming_a_missing_orchestrator() {
    // The pack's orchestration error must land in the session summary honestly — not as
    // "no orchestrator is attached". Hub path: reaction returns Dispatched immediately; the
    // pack fails the session with the real error.
    use liberado_common::{DispatchAction, DispatchDecision};
    use liberado_config_loader::DispatchTuning;
    use liberado_dispatch_pack::DispatchPack;
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};
    use liberado_provider::{CompletionResponse, MockProvider};
    use liberado_session::{GoalSessionHub, GoalSessionStore, SessionStatus};
    use std::sync::Arc;

    struct FailingFactory;
    #[async_trait::async_trait]
    impl RuntimeFactory for FailingFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Err(RuntimeSetupError(
                "Connection failed: error sending request for url (http://127.0.0.1:3737/mcp)"
                    .into(),
            ))
        }
    }

    let (daemon, _dir) = temp_daemon().await;
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.95,
        rationale: "routine".into(),
    };
    // Give the pool a grant so ExecuteDirect tries the factory (empty grant uses NoMcpRuntime).
    let pool_caps = CapabilitySet::from_iter([liberado_common::Capability::ExecuteMcp("x".into())]);
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
        Arc::new(MockProvider::with_script("exec", Vec::new())),
        FailingFactory,
        pool_caps.clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "default",
    );

    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool("default", dispatcher, orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            pool_caps,
            Vec::new(),
        )
        .with_goal_hub(hub.clone());

    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(200)).await;
    sender
        .send(Event::trigger(
            "CronFired",
            "cron:nightly",
            "cron:nightly:1",
            liberado_common::EventPayload {
                summary: Some("say hello".into()),
                ..Default::default()
            },
        ))
        .unwrap();
    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed");
    let session_id = match reaction.outcome {
        ReactionOutcome::Dispatched { session_id } => session_id,
        other => panic!("expected Dispatched, got {}", other.label()),
    };
    let snap = hub.await_terminal(&session_id).await.unwrap();
    assert_eq!(snap.session.status, SessionStatus::Failed);
    let summary = &snap.session.result.as_ref().unwrap().summary;
    assert!(
        summary.contains("orchestration failed") || summary.contains("Connection failed"),
        "the summary must say execution was attempted and failed, got: {summary}"
    );
    assert!(
        !summary.contains("no orchestrator"),
        "an orchestrator IS attached — blaming a missing one sends the human hunting a \
             config bug that does not exist: {summary}"
    );

    handle.abort();
}

/// Never actually builds a runtime (the Clarify path stops before execution) — exists only to
/// satisfy `Orchestrator::new`'s type.
struct NoopFactoryForClarify;
#[async_trait::async_trait]
impl liberado_executor::RuntimeFactory for NoopFactoryForClarify {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: WriteProvenance,
    ) -> Result<Box<dyn liberado_executor::ToolRuntime>, liberado_executor::RuntimeSetupError> {
        unreachable!("a Clarify never reaches execution")
    }
}

#[tokio::test]
async fn pools_are_authority_segregated() {
    // The direct proof that named pools (Decision 18 checkpoint #3) aren't just routed but
    // actually authority-segregated: two pools, two schedules, both decisions asking to call
    // the SAME MCP — but only "granted-pool" was actually given that capability. If pools
    // shared authority (e.g. a bug reusing one capability set for both), "blocked-pool" would
    // reach the real runtime too; it must not.
    use liberado_common::{Capability, DispatchAction, DispatchDecision, EventPayload};
    use liberado_config_loader::DispatchTuning;
    use liberado_executor::SUBMIT_REPORT_TOOL;
    use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
    use liberado_test_support::CallRecordingFactory;

    let (daemon, _dir) = temp_daemon().await;

    // Both pools' dispatchers classify identically: ExecuteDirect against "shared-mcp".
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["shared-mcp".into()],
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    let decision_json = serde_json::to_string(&decision).unwrap();
    let dispatcher_for = || {
        Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "dispatch",
                [CompletionResponse::text(decision_json.clone())],
            )),
            DispatchTuning::default(),
            4,
        )
    };

    // granted-pool: actually holds the "shared-mcp" capability, so its orchestrator's
    // ExecuteDirect scoping resolves a non-empty `allowed_mcps` and reaches the real factory.
    let granted_capabilities =
        CapabilitySet::from_iter([Capability::ExecuteMcp("shared-mcp".into())]);
    let granted_factory = CallRecordingFactory::default();
    let granted_calls = granted_factory.calls.clone();
    let granted_exec = Arc::new(MockProvider::with_script(
        "exec-granted",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c1",
                "shared-mcp:do_thing",
                serde_json::json!({}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c2",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "granted pool acted" }),
            )]),
        ],
    ));
    let granted_orch = Orchestrator::new(
        granted_exec,
        granted_factory,
        granted_capabilities.clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "granted-pool",
    );

    // blocked-pool: an EMPTY capability set — the dispatcher's own pre-flight capability guard
    // (`guards::evaluate`'s `CapabilityGap` check, run against THIS pool's own capabilities)
    // catches the reference to "shared-mcp" before the decision ever reaches an orchestrator,
    // downgrading it to Clarify — so `blocked_exec`/`blocked_factory` below must NEVER be
    // touched at all. That's the segregation proof: the identical decision that runs for real
    // in granted-pool never even reaches execution in blocked-pool.
    let blocked_factory = CallRecordingFactory::default();
    let blocked_calls = blocked_factory.calls.clone();
    let blocked_exec = Arc::new(MockProvider::with_script("exec-blocked", []));
    let blocked_orch = Orchestrator::new(
        blocked_exec,
        blocked_factory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "blocked-pool",
    );

    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_pool_dispatcher(
            "granted-pool",
            dispatcher_for(),
            Arc::new(CapabilityCatalog::new()),
            granted_capabilities,
        )
        .with_pool_orchestrator("granted-pool", granted_orch)
        .with_pool_dispatcher(
            "blocked-pool",
            dispatcher_for(),
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
        )
        .with_pool_orchestrator("blocked-pool", blocked_orch);

    // Inject one event per pool directly (the same seam `liberado-server`'s webhook handler
    // and `liberado-cron` both use) — deterministic, no dependence on real-time cron ticking.
    let sender = daemon.event_sender();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(200)).await;
    sender
        .send(Event::trigger(
            "Trigger",
            "test:granted",
            "test:granted:1",
            EventPayload {
                pool: Some("granted-pool".into()),
                ..Default::default()
            },
        ))
        .unwrap();
    sender
        .send(Event::trigger(
            "Trigger",
            "test:blocked",
            "test:blocked:1",
            EventPayload {
                pool: Some("blocked-pool".into()),
                ..Default::default()
            },
        ))
        .unwrap();

    let mut outcomes_by_pool = std::collections::HashMap::new();
    for _ in 0..2 {
        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for a reaction")
            .expect("reaction channel closed");
        outcomes_by_pool.insert(reaction.event.payload.pool.clone(), reaction.outcome);
    }

    // granted-pool: authorized for "shared-mcp" — the decision runs for real.
    match outcomes_by_pool.get(&Some("granted-pool".to_string())) {
        Some(ReactionOutcome::Acted(Disposition::Reported(_))) => {}
        Some(o) => panic!("expected granted-pool to reach Reported, got {}", o.label()),
        None => panic!("no reaction recorded for granted-pool"),
    }

    // blocked-pool: an identical decision naming the same MCP, but this pool was never
    // granted it — the dispatcher's own pre-flight guard catches it and downgrades to
    // Clarify, never reaching an orchestrator/runtime at all.
    match outcomes_by_pool.get(&Some("blocked-pool".to_string())) {
        Some(ReactionOutcome::Acted(Disposition::Clarify { what_blocked, .. })) => {
            assert_eq!(*what_blocked, liberado_common::BlockReason::CapabilityGap);
        }
        Some(o) => panic!(
            "expected blocked-pool to be guard-downgraded to Clarify, got {}",
            o.label()
        ),
        None => panic!("no reaction recorded for blocked-pool"),
    }

    // The load-bearing assertion: granted-pool's own capability actually reached the real
    // runtime; blocked-pool's identical request never did, despite an identical decision.
    assert_eq!(
        granted_calls.lock().unwrap().len(),
        1,
        "granted-pool must reach the real runtime for a call it's actually authorized for"
    );
    assert!(
        blocked_calls.lock().unwrap().is_empty(),
        "blocked-pool must NEVER reach the real runtime for an MCP it wasn't granted, even \
             though the decision asked for the exact same call granted-pool made"
    );

    handle.abort();
}

#[tokio::test]
async fn event_sender_lets_an_external_producer_inject_an_event() {
    // The seam `liberado-server`'s webhook handler uses: grab a sender before `run()` moves
    // `self`, then push an `Event` in from completely outside any `EventSource` — no cron, no
    // vault change, just a direct injection — and it must still flow through `react()` exactly
    // like any other source.
    use liberado_common::EventPayload;

    let (daemon, _dir) = temp_daemon().await;
    let sender = daemon.event_sender();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(200)).await;
    sender
        .send(Event::trigger(
            "WebhookFired",
            "webhook:test-hook",
            "webhook:test-hook:1",
            EventPayload {
                summary: Some("an externally-injected goal".into()),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for the injected event's reaction")
        .expect("reaction channel closed");

    assert_eq!(reaction.event.source, "webhook:test-hook");
    // No dispatcher attached in this test daemon — watch-only, so Observed — the point here is
    // only that the injected event reached `react()` at all, not what it decided.
    assert!(matches!(reaction.outcome, ReactionOutcome::Observed));

    handle.abort();
}

#[tokio::test]
async fn daemon_acts_on_a_decision_via_the_orchestrator() {
    use liberado_common::{DispatchAction, DispatchDecision, Outcome};
    use liberado_config_loader::DispatchTuning;
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
    use std::sync::Arc;

    // A tool runtime + factory that need no real MCP (the scripted model just files a report).
    struct NoopRuntime;
    #[async_trait::async_trait]
    impl ToolRuntime for NoopRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    struct NoopFactory;
    #[async_trait::async_trait]
    impl RuntimeFactory for NoopFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: liberado_common::WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(NoopRuntime))
        }
    }

    let (daemon, dir) = temp_daemon().await;

    // Dispatcher classifies to ExecuteDirect (no MCPs referenced → passes the guards).
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.95,
        rationale: "trivial".into(),
    };
    let dispatch_provider = Arc::new(MockProvider::with_script(
        "dispatch",
        [CompletionResponse::text(
            serde_json::to_string(&decision).unwrap(),
        )],
    ));
    let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

    // The orchestrator's executor: the model immediately files a report.
    let exec_provider = Arc::new(MockProvider::with_script(
        "exec",
        [CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "c",
            SUBMIT_REPORT_TOOL,
            serde_json::json!({ "outcome": "succeeded", "summary": "done" }),
        )])],
    ));
    let orchestrator = Orchestrator::new(
        exec_provider,
        NoopFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "default",
    );

    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_dispatcher(
            dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_orchestrator(orchestrator);

    let vault_dir = dir.path().to_path_buf();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(vault_dir.join("act.md"), "do something").unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed");

    let ReactionOutcome::Acted(Disposition::Reported(report)) = reaction.outcome else {
        panic!("expected Acted/Reported, got {}", reaction.outcome.label());
    };
    assert_eq!(report.outcome, Outcome::Succeeded);

    handle.abort();
}

#[tokio::test]
async fn daemon_emits_a_proposal_for_a_high_consequence_action() {
    use liberado_common::{
        Capability, Consequence, DispatchAction, DispatchDecision, Proposal, ProposalStatus,
        ProposedAction, ToolCall,
    };
    use liberado_config_loader::DispatchTuning;
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
    use std::sync::Arc;

    // The orchestrator never builds a runtime for a Propose; this factory just satisfies the type.
    struct UnusedRuntime;
    #[async_trait::async_trait]
    impl ToolRuntime for UnusedRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    struct UnusedFactory;
    #[async_trait::async_trait]
    impl RuntimeFactory for UnusedFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(UnusedRuntime))
        }
    }

    let (daemon, dir) = temp_daemon().await;

    // Classifier picks a concrete external action: an ExecuteDirect with an `email:send` seed
    // call. Granted + confident, but External → the consequence gate downgrades it to Propose.
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "email:send".into(),
                args: serde_json::json!({ "to": "boss@example.com" }),
            }],
            relevant_mcps: Vec::new(),
        },
        confidence: 0.95,
        rationale: "send the requested email".into(),
    };
    let dispatch_provider = Arc::new(MockProvider::with_script(
        "dispatch",
        [CompletionResponse::text(
            serde_json::to_string(&decision).unwrap(),
        )],
    ));
    let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

    // Catalog declares the External MCP; capabilities grant it so the only block is consequence.
    let catalog = CapabilityCatalog::new();
    catalog.register(McpDescriptor {
        name: "email".into(),
        description: "send email".into(),
        consequence: Consequence::External,
        provenance: None,
        ..Default::default()
    });
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("email".into())]);
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script("exec", Vec::new())),
        UnusedFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "default",
    );

    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_dispatcher(dispatcher, Arc::new(catalog), capabilities, Vec::new())
        .with_orchestrator(orchestrator);

    let vault_dir = dir.path().to_path_buf();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(vault_dir.join("email-me.md"), "please email the boss").unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed");

    let ReactionOutcome::Acted(Disposition::Propose(proposal)) = reaction.outcome else {
        panic!("expected Acted/Propose, got {}", reaction.outcome.label());
    };

    // The proposal artifact landed in the vault and round-trips back to a Pending proposal
    // carrying the email tool call.
    let proposals_dir = vault_dir.join("proposals");
    let entries: Vec<_> = std::fs::read_dir(&proposals_dir)
        .expect("proposals/ should exist")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one proposal note");
    let contents = std::fs::read_to_string(&entries[0]).unwrap();
    let parsed = Proposal::from_note(&contents).expect("proposal note round-trips");
    assert_eq!(&parsed, proposal.as_proposal());
    assert_eq!(parsed.status, ProposalStatus::Pending);
    match parsed.proposed_action {
        ProposedAction::ToolCalls(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].tool, "email:send");
        }
        other => panic!("expected ToolCalls, got {other:?}"),
    }

    handle.abort();
}

#[tokio::test]
async fn daemon_downgrades_a_zone_restricted_seed_call_to_a_proposal() {
    // End-to-end regression for the zone-write-class guard (§6 #2) at the daemon level: unit
    // tests already cover `dispatcher::guards::evaluate` and `RiskGatedToolRuntime` separately
    // (they share `liberado_common::zone_write_restriction` so they can't drift on the
    // determination logic itself), but nothing previously proved the daemon actually threads a
    // configured `zone_write_classes` through `with_dispatcher` into a real reaction
    // (`docs/roadmap/hygiene-audit-2026-07-05.md` P3.4). The MCP's own `consequence` is
    // `Reversible` — below the consequence gate — so if a proposal is emitted here, the zone
    // restriction is provably what caused it, not the (separately already-tested) consequence
    // check.
    use liberado_common::{
        Capability, Consequence, DispatchAction, DispatchDecision, Proposal, ProposalStatus,
        ProposedAction, ToolCall,
    };
    use liberado_config_loader::DispatchTuning;
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, ToolRuntime};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
    use std::sync::Arc;

    // The orchestrator never builds a runtime for a Propose; this factory just satisfies the type.
    struct UnusedRuntime;
    #[async_trait::async_trait]
    impl ToolRuntime for UnusedRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    struct UnusedFactory;
    #[async_trait::async_trait]
    impl RuntimeFactory for UnusedFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(UnusedRuntime))
        }
    }

    let (daemon, dir) = temp_daemon().await;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "vault-mcp:write_note".into(),
                args: serde_json::json!({ "path": "reviews/q1.md" }),
            }],
            relevant_mcps: Vec::new(),
        },
        confidence: 0.95,
        rationale: "file the review note".into(),
    };
    let dispatch_provider = Arc::new(MockProvider::with_script(
        "dispatch",
        [CompletionResponse::text(
            serde_json::to_string(&decision).unwrap(),
        )],
    ));
    let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

    // Reversible (not Irreversible/External), granted by capability, and targets a zone this
    // pool has restricted to ProposalOnly — the only thing that should block direct execution.
    let catalog = CapabilityCatalog::new();
    catalog.register(McpDescriptor {
        name: "vault-mcp".into(),
        description: "vault note writer".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: Some("reviews".into()),
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    });
    let capabilities = CapabilitySet::from_iter([Capability::ExecuteMcp("vault-mcp".into())]);
    let zone_write_classes = vec![("reviews".to_string(), WriteClass::ProposalOnly)];
    let orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script("exec", Vec::new())),
        UnusedFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "default",
    );

    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_dispatcher(
            dispatcher,
            Arc::new(catalog),
            capabilities,
            zone_write_classes,
        )
        .with_orchestrator(orchestrator);

    let vault_dir = dir.path().to_path_buf();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(vault_dir.join("review-me.md"), "please file this review").unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed");

    let ReactionOutcome::Acted(Disposition::Propose(proposal)) = reaction.outcome else {
        panic!("expected Acted/Propose, got {}", reaction.outcome.label());
    };

    let proposals_dir = vault_dir.join("proposals");
    let entries: Vec<_> = std::fs::read_dir(&proposals_dir)
        .expect("proposals/ should exist")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one proposal note");
    let contents = std::fs::read_to_string(&entries[0]).unwrap();
    let parsed = Proposal::from_note(&contents).expect("proposal note round-trips");
    assert_eq!(&parsed, proposal.as_proposal());
    assert_eq!(parsed.status, ProposalStatus::Pending);
    match parsed.proposed_action {
        ProposedAction::ToolCalls(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].tool, "vault-mcp:write_note");
        }
        other => panic!("expected ToolCalls, got {other:?}"),
    }

    handle.abort();
}

#[tokio::test]
async fn daemon_executes_an_approved_proposal() {
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::MockProvider;
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<liberado_provider::CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());

    // Pre-create proposals/ so the watcher doesn't react to directory creation.
    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    // Let the watcher establish before writing the proposal file.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Write an approved proposal (simulates a human editing status to approved), signed with
    // the same key the daemon verifies against.
    let proposal = Proposal::pending(
        "vault-change:test-proposal:abc",
        "vault-change:test-proposal:abc",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "test task" }),
        }]),
        "a test proposal",
    );
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("approved.md"), proposal.to_note()).unwrap();

    // Wait for the daemon to react to the proposal change.
    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    // (a) The runtime recorded the approved tool invocation. Scope the guard so it is not held
    //     across the archive-polling awaits below.
    {
        let recorded = invoked.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "approved proposal must execute the tool call"
        );
        assert_eq!(recorded[0].name, "tasks:create");
    }

    // (b) The proposal note was flipped to Done and archived out of the active dir (Gap 1):
    //     the original path is gone, and the Done note now lives under archive/approved/.
    //     Poll for the move to land (the archive is a second vault write after the reaction).
    let archived = proposals_dir.join("archive/approved/approved.md");
    let mut contents = None;
    for _ in 0..50 {
        if archived.exists() {
            contents = Some(std::fs::read_to_string(&archived).unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let contents = contents.expect("approved proposal must be archived under archive/approved/");
    assert!(
        !proposals_dir.join("approved.md").exists(),
        "archived proposal must be removed from the active proposals dir"
    );
    let parsed = Proposal::from_note(&contents).unwrap();
    assert_eq!(parsed.status, ProposalStatus::Done);

    handle.abort();
}

#[tokio::test]
async fn daemon_archives_a_rejected_proposal() {
    // A human deny (Telegram/Obsidian flips status to Rejected, a real human write) is observed
    // and filed into archive/rejected/ — no orchestrator needed, since a terminal proposal is
    // never executed. Proves the Gap 1 terminal-observe hook, distinct from the approve path.
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};

    let signer = ProposalSigner::random();
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    // A signed proposal the human then rejected. Signed with the daemon's key so the integrity
    // check (step 2.5) passes and we reach the terminal-observe branch.
    let proposal = Proposal::pending(
        "vault-change:rejected-proposal:xyz",
        "vault-change:rejected-proposal:xyz",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "denied task" }),
        }]),
        "a rejected proposal",
    );
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Rejected);
    std::fs::write(proposals_dir.join("rejected.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    // The rejected note moved to archive/rejected/, leaving the active dir clean.
    let archived = proposals_dir.join("archive/rejected/rejected.md");
    let mut contents = None;
    for _ in 0..50 {
        if archived.exists() {
            contents = Some(std::fs::read_to_string(&archived).unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let contents = contents.expect("rejected proposal must be archived under archive/rejected/");
    assert!(
        !proposals_dir.join("rejected.md").exists(),
        "archived proposal must be removed from the active proposals dir"
    );
    assert_eq!(
        Proposal::from_note(&contents).unwrap().status,
        ProposalStatus::Rejected
    );

    handle.abort();
}

#[tokio::test]
async fn handle_proposal_change_expires_and_archives_past_deadline_pending() {
    // A human touch of a still-Pending note past `expires` must not wait for the reaper: flip
    // status to Expired and archive under archive/expired/.
    use chrono::{Duration as ChronoDuration, Utc};
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};

    let signer = ProposalSigner::random();
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon.with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let mut proposal = Proposal::pending(
        "vault-change:stale-pending:1",
        "vault-change:stale-pending:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "too late" }),
        }]),
        "expired pending proposal",
    );
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(1));
    let proposal = signer.sign(proposal);
    let rel = Path::new("proposals/stale-pending.md");
    let prov = WriteProvenance::agent("test", "c1");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &prov)
        .await
        .unwrap();

    let outcome = daemon.handle_proposal_change(rel).await.unwrap();
    assert!(
        matches!(outcome, ReactionOutcome::Observed),
        "past-deadline notes are observed, never executed"
    );

    assert!(
        daemon.vault.read(rel).await.is_err(),
        "expired proposal must leave the active proposals/ dir"
    );
    let archived = daemon
        .vault
        .read("proposals/archive/expired/stale-pending.md")
        .await
        .expect("must land under archive/expired/");
    assert_eq!(
        Proposal::from_note(&archived).unwrap().status,
        ProposalStatus::Expired
    );
}

#[tokio::test]
async fn handle_proposal_change_does_not_execute_approved_past_deadline() {
    // Late approve after `expires` must not run tools — only expire + archive.
    use chrono::{Duration as ChronoDuration, Utc};
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::MockProvider;
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<liberado_provider::CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let mut proposal = Proposal::pending(
        "vault-change:late-approve:1",
        "vault-change:late-approve:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should never run" }),
        }]),
        "approved after expiry",
    );
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(1));
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    let rel = Path::new("proposals/late-approve.md");
    let prov = WriteProvenance::agent("test", "c1");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &prov)
        .await
        .unwrap();

    let outcome = daemon.handle_proposal_change(rel).await.unwrap();
    assert!(matches!(outcome, ReactionOutcome::Observed));

    assert!(
        invoked.lock().unwrap().is_empty(),
        "past-deadline approved proposal must never invoke tools"
    );
    assert!(daemon.vault.read(rel).await.is_err());
    let archived = daemon
        .vault
        .read("proposals/archive/expired/late-approve.md")
        .await
        .expect("late approve must archive as expired, not approved");
    assert_eq!(
        Proposal::from_note(&archived).unwrap().status,
        ProposalStatus::Expired
    );
}

#[tokio::test]
async fn daemon_does_not_execute_a_pending_proposal() {
    use liberado_common::{Proposal, ProposalStatus, ProposedAction, ToolCall};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::MockProvider;
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let orch = Orchestrator::new(
        std::sync::Arc::new(MockProvider::with_script(
            "mock",
            Vec::<liberado_provider::CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch);

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Write a PENDING proposal (not approved).
    let proposal = Proposal::pending(
        "pending-test",
        "pending-correlation",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should-not-run" }),
        }]),
        "a pending proposal",
    );
    std::fs::write(proposals_dir.join("pending-test.md"), proposal.to_note()).unwrap();

    // Wait for the daemon to process the change (reaction should arrive quickly).
    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    // Runtime recorded nothing — the pending proposal is not actionable.
    let recorded = invoked.lock().unwrap();
    assert!(
        recorded.is_empty(),
        "pending proposal must NOT invoke any tool"
    );

    // Proposal status is still Pending.
    let contents = std::fs::read_to_string(proposals_dir.join("pending-test.md")).unwrap();
    let parsed = Proposal::from_note(&contents).unwrap();
    assert_eq!(parsed.status, ProposalStatus::Pending);

    handle.abort();
}

#[tokio::test]
async fn daemon_rejects_an_approved_proposal_with_a_bad_integrity_signature() {
    // Same shape as `daemon_executes_an_approved_proposal`, but the note is signed with a
    // DIFFERENT key than the daemon verifies against — simulating a wholesale-forged proposal
    // (or a legitimate one whose proposed_action was tampered with after signing). Must not
    // execute, and must not be marked done — left alone so a human can investigate.
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::MockProvider;
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let daemon_signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<liberado_provider::CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        daemon_signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(daemon_signer);

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Signed with an unrelated key, not the daemon's — a forged or tampered signature either
    // way, from the daemon's point of view.
    let forging_signer = ProposalSigner::random();
    let proposal = Proposal::pending(
        "forged-test",
        "forged-correlation",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should-not-run" }),
        }]),
        "a forged approval",
    );
    let mut proposal = forging_signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("forged.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    assert!(
        invoked.lock().unwrap().is_empty(),
        "a proposal with a bad integrity signature must NOT invoke any tool, even though \
             status is approved"
    );

    // Left as Approved (not silently flipped to Done) — a real failure state a human should
    // notice, not one indistinguishable from a successful run.
    let contents = std::fs::read_to_string(proposals_dir.join("forged.md")).unwrap();
    let parsed = Proposal::from_note(&contents).unwrap();
    assert_eq!(
        parsed.status,
        ProposalStatus::Approved,
        "a rejected-for-integrity proposal must not be marked Done"
    );

    handle.abort();
}

#[tokio::test]
async fn runtime_gated_downgrade_lands_in_the_vault_and_executes_once_approved() {
    // End-to-end proof of item 3's fix: a RiskGatedToolRuntime downgrade writes into the
    // *vault's* proposals/ directory (not a dead-end data dir), and approving it there actually
    // executes it via the same daemon pipeline pre-flight proposals already use.
    use liberado_common::{Capability, Consequence, Proposal, ProposalSigner, ProposalStatus};
    use liberado_executor::{RiskGatedToolRuntime, ToolRuntime};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::{MockProvider, ToolInvocation};
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

    let (daemon, dir) = temp_daemon().await;
    let vault_path = dir.path().to_path_buf();
    let signer = ProposalSigner::random();

    // 1. A RiskGatedToolRuntime downgrades a high-consequence call, writing a proposal straight
    //    into this vault's proposals/ directory (proposals_dir = the vault root — write_proposal
    //    joins "proposals" itself, matching the daemon's own PROPOSALS_DIR convention).
    let inner: Arc<dyn ToolRuntime> = Arc::new(InvocationRecordingRuntime::default());
    let gated = RiskGatedToolRuntime::new(
        inner,
        CapabilitySet::from_iter([Capability::ExecuteMcp("dangerous-mcp".into())]),
        vec![("dangerous-mcp".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        vault_path.clone(),
        "clean up the vault".into(),
        "runtime-gate-test".into(),
        signer.clone(),
        "default",
    );
    let call = ToolInvocation::new(
        "c1",
        "dangerous-mcp:wipe",
        serde_json::json!({ "path": "everything" }),
    );
    let downgrade_msg = gated.invoke(&call).await.expect("downgrade is Ok, not Err");
    assert!(downgrade_msg.contains("PROPOSAL CREATED"));

    let proposals_dir = vault_path.join("proposals");
    let mut entries: Vec<_> = std::fs::read_dir(&proposals_dir)
        .expect("the runtime-level downgrade must have created the vault's proposals/ dir")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(entries.len(), 1, "exactly one proposal file should exist");
    let proposal_path = entries.remove(0).path();
    let written = Proposal::from_note(&std::fs::read_to_string(&proposal_path).unwrap())
        .expect("proposal note round-trips");
    assert_eq!(written.status, ProposalStatus::Pending);
    assert!(signer.verify(&written), "the downgrade must be signed");

    // 2. Now wire up the daemon (matching signer + an orchestrator that records invocations) and
    //    let it observe a human's approval edit — the exact same pipeline pre-flight proposals
    //    already use, proving this is no longer a dead end.
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<liberado_provider::CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(signer);

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The human approves — status flips, signature (over the unchanged action/id/etc.) stays
    // valid.
    let mut approved = written;
    approved.status = ProposalStatus::Approved;
    std::fs::write(&proposal_path, approved.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    let recorded = invoked.lock().unwrap();
    assert_eq!(
        recorded.len(),
        1,
        "approving the runtime-gated proposal must actually execute it"
    );
    assert_eq!(recorded[0].name, "dangerous-mcp:wipe");

    handle.abort();
}

/// An event that names a session profile missing from the boot map must not start a hosted
/// session (fail closed — never silent full-pool fallback).
#[tokio::test]
async fn unknown_session_profile_does_not_start_session() {
    use liberado_common::{Capability, CapabilitySet, EventPayload};
    use liberado_config_loader::DispatchTuning;
    use liberado_dispatch_pack::DispatchPack;
    use liberado_provider::{CompletionResponse, MockProvider};
    use liberado_session::{GoalSessionHub, GoalSessionStore};
    use std::collections::HashMap;
    use std::sync::Arc;

    let (daemon, _dir) = temp_daemon().await;
    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    // Hub present so the profile branch runs (no hub → inline path, no profile grant).
    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    );
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let mut known = HashMap::new();
    known.insert(
        "interactive-cron".into(),
        CapabilitySet::from_iter([Capability::AskHuman]),
    );

    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_session_profile_caps(known)
        .with_goal_hub(hub.clone());

    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(50)).await;

    sender
        .send(Event::trigger(
            "CronFired",
            "cron:bad-profile",
            "cron:bad-profile:1",
            EventPayload {
                summary: Some("should not run".into()),
                data: serde_json::json!({ "profile": "typo-not-in-map" }),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");
    assert!(
        matches!(reaction.outcome, ReactionOutcome::Observed),
        "unknown profile must be Observed, not Dispatched; got {}",
        reaction.outcome.label()
    );
    assert!(
        hub.list().await.is_empty(),
        "no hosted session may be created for an unknown profile"
    );

    handle.abort();
}

/// A known profile name resolves and still dispatches a hosted session.
#[tokio::test]
async fn known_session_profile_still_dispatches() {
    use liberado_common::{
        Capability, CapabilitySet, DispatchAction, DispatchDecision, EventPayload,
    };
    use liberado_config_loader::DispatchTuning;
    use liberado_dispatch_pack::DispatchPack;
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
    use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
    use liberado_session::{GoalSessionHub, GoalSessionStore};
    use std::collections::HashMap;
    use std::sync::Arc;

    struct NoopRuntime;
    #[async_trait::async_trait]
    impl ToolRuntime for NoopRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    struct NoopFactory;
    #[async_trait::async_trait]
    impl RuntimeFactory for NoopFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(NoopRuntime))
        }
    }

    let (daemon, _dir) = temp_daemon().await;
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.95,
        rationale: "profile ok".into(),
    };
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
        Arc::new(MockProvider::with_script(
            "exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "ok" }),
            )])],
        )),
        NoopFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "default",
    );
    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool("default", dispatcher, orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let mut known = HashMap::new();
    known.insert(
        "interactive-cron".into(),
        CapabilitySet::from_iter([Capability::AskHuman]),
    );

    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_session_profile_caps(known)
        .with_goal_hub(hub.clone());

    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(50)).await;

    sender
        .send(Event::trigger(
            "CronFired",
            "cron:good-profile",
            "cron:good-profile:1",
            EventPayload {
                summary: Some("profile-gated goal".into()),
                data: serde_json::json!({ "profile": "interactive-cron" }),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");
    let session_id = match reaction.outcome {
        ReactionOutcome::Dispatched { session_id } => session_id,
        other => panic!("known profile must dispatch, got {}", other.label()),
    };
    let snap = hub
        .snapshot(&session_id)
        .await
        .expect("session must exist on hub");
    assert_eq!(
        snap.session.goal.profile.as_deref(),
        Some("interactive-cron")
    );

    handle.abort();
}

// ── L9 — cron/webhook-class event → joinable Dispatched session (T1 suite) ──

/// L9 (docs/roadmap/live-conformance-suite.md): a cron/webhook-class event on the **shipped**
/// daemon reaction path becomes `ReactionOutcome::Dispatched { session_id }` with a **joinable**
/// hosted session on the hub (snapshot succeeds; domain is the reaction/dispatch pack), not an
/// unrecorded inline-only reaction.
#[tokio::test]
async fn l9_cron_event_becomes_joinable_dispatched_session() {
    use liberado_common::{DispatchAction, DispatchDecision};
    use liberado_config_loader::DispatchTuning;
    use liberado_dispatch_pack::{DISPATCH_DOMAIN, DispatchPack};
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
    use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
    use liberado_session::{
        GoalSessionHub, GoalSessionStore, SessionStatus, TerminalKind, Visibility,
    };
    use std::sync::Arc;

    struct NoopRuntime;
    #[async_trait::async_trait]
    impl ToolRuntime for NoopRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    struct NoopFactory;
    #[async_trait::async_trait]
    impl RuntimeFactory for NoopFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(NoopRuntime))
        }
    }

    let (daemon, _dir) = temp_daemon().await;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
        },
        confidence: 0.95,
        rationale: "L9 routine cron".into(),
    };
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
        Arc::new(MockProvider::with_script(
            "exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "L9 brief delivered" }),
            )])],
        )),
        NoopFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
        "default",
    );

    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        std::env::temp_dir(),
    )
    .with_pool("default", dispatcher, orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    // Pool dispatcher is required so `react` does not short-circuit to Observed before the hub.
    let grant_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let daemon = daemon
        .with_dispatcher(
            grant_dispatcher,
            Arc::new(CapabilityCatalog::new()),
            CapabilitySet::empty(),
            Vec::new(),
        )
        .with_goal_hub(hub.clone());

    // Production external inject path (webhooks use the same `event_sender` + `run` loop as cron).
    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(50)).await;

    sender
        .send(Event::trigger(
            "CronFired",
            "cron:l9-morning",
            "cron:l9-morning:t1",
            liberado_common::EventPayload {
                summary: Some("L9 morning brief goal".into()),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for L9 reaction")
        .expect("reaction channel closed");
    assert_eq!(reaction.event.source, "cron:l9-morning");

    let session_id = match reaction.outcome {
        ReactionOutcome::Dispatched { session_id } => session_id,
        other => panic!(
            "L9 requires Dispatched {{ session_id }}, got {}",
            other.label()
        ),
    };

    // Joinable: hub can resolve the id immediately (hosted session, not narration-only).
    let live = hub
        .snapshot(&session_id)
        .await
        .expect("L9: Dispatched session_id must be joinable on the hub");
    assert_eq!(
        live.session.goal.domain.as_str(),
        DISPATCH_DOMAIN,
        "reaction sessions run under the dispatch domain pack"
    );
    assert_eq!(live.session.visibility, Visibility::Background);
    assert_eq!(
        live.session.goal.description, "L9 morning brief goal",
        "session must carry the event goal text, not an empty shell"
    );

    let snap = hub
        .await_terminal(&session_id)
        .await
        .expect("L9 session should reach terminal");
    assert_eq!(snap.session.status, SessionStatus::Succeeded);
    assert_eq!(
        snap.session.result.as_ref().unwrap().summary,
        "L9 brief delivered"
    );
    assert_eq!(
        snap.session.result.as_ref().unwrap().terminal,
        TerminalKind::Succeeded
    );

    handle.abort();
}
