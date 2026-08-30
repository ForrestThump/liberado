//! Tests for daemon reactions, dispatcher routing, cron interchangeability, and orchestrator execution.

use super::super::*;
use super::test_fixtures::*;
use liberado_common::{
    BlockReason, CapabilityCatalog, CapabilitySet, Delivery, DispatchAction, DispatchDecision,
    Event, Outcome, WriteProvenance, event_source,
};
use liberado_config_loader::DispatchTuning;
use liberado_cron::{CronEventSource, Schedule};
use liberado_dispatch_pack::{DISPATCH_DOMAIN, DispatchPack};
use liberado_dispatcher::Dispatcher;
use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
use liberado_orchestrator::{Disposition, Orchestrator};
use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
use liberado_session::{
    GoalSessionHub, GoalSessionStore, SessionEventKind, SessionStatus, TerminalKind, Visibility,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

#[tokio::test]
async fn daemon_routes_reaction_through_dispatcher() {
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
    std::fs::create_dir_all(vault_dir.join("inbox")).unwrap();
    std::fs::write(vault_dir.join("inbox/idea.md"), "a captured thought").unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    assert_eq!(
        reaction.event.payload.path.as_deref(),
        Some("inbox/idea.md")
    );
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
        deliver: None,
        max_turns: None,
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
    std::fs::create_dir_all(vault_dir.join("inbox")).unwrap();
    std::fs::write(vault_dir.join("inbox/idea.md"), "a captured thought").unwrap();

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
    let (daemon, _dir) = temp_daemon().await;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
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
        deliver: None,
        max_turns: None,
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
            delivery: Delivery::Summarize,
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

#[tokio::test]
async fn daemon_acts_on_a_decision_via_the_orchestrator() {
    let (daemon, dir) = temp_daemon().await;

    // Dispatcher classifies to ExecuteDirect (no MCPs referenced → passes the guards).
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
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
    std::fs::create_dir_all(vault_dir.join("inbox")).unwrap();
    std::fs::write(vault_dir.join("inbox/act.md"), "do something").unwrap();

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
async fn stamp_local_time_is_attached_for_cron_events() {
    use liberado_common::UserTimezone;

    let (mut daemon, _dir) = temp_daemon().await;
    daemon = daemon.with_user_timezone(UserTimezone::default());

    let event = Event::trigger("CronFired", "cron:test", "cron:test:1", Default::default());
    assert!(
        daemon
            .stamp_local_time_if_needed(&event, "a goal")
            .is_some(),
        "cron events without a vault path should get a time stamp"
    );

    let event = Event::trigger(
        "NoteChanged",
        event_source::TURBOVAULT_SUBSCRIPTION,
        "vault-change:1",
        liberado_common::EventPayload {
            path: Some("note.md".into()),
            ..Default::default()
        },
    );
    assert!(
        daemon
            .stamp_local_time_if_needed(&event, "a goal")
            .is_none(),
        "vault change events with a path should NOT get a time stamp"
    );

    let (daemon_no_tz, _dir2) = temp_daemon().await;
    let event = Event::trigger("CronFired", "cron:test", "cron:test:2", Default::default());
    assert!(
        daemon_no_tz
            .stamp_local_time_if_needed(&event, "a goal")
            .is_none(),
        "no timezone configured → no stamp"
    );
}
