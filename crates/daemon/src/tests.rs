use super::*;
use crate::helpers::*;
use liberado_common::{Delivery, McpDescriptor, WriteProvenance, event_source};
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

/// `watcher_health()` starts false and is only true once `run()` has actually spawned the watch.
///
/// `GET /api/status` used to answer this with the literal `true`, so every dashboard asserted a
/// live capture pipeline whether or not one was running — which reads as "the pipeline broke" to
/// anyone debugging, rather than "it was never started". The flag is only worth anything if it can
/// be false, so that is what this pins.
#[tokio::test]
async fn watcher_health_is_false_before_run_spawns_the_watch() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open("test", dir.path()).await.unwrap();
    let health = daemon.watcher_health();
    assert!(
        !health.load(std::sync::atomic::Ordering::Relaxed),
        "a daemon that has not run yet is not watching anything"
    );
    // The handle is shared, not a snapshot — the surface holds this across `run()` taking `self`.
    assert!(std::sync::Arc::ptr_eq(&health, &daemon.watcher_health()));
}

/// A schedule's declared ceiling reaches the pack as `GoalSpec::max_turns`.
///
/// The daemon builds the goal from the event alone, so if the payload stopped carrying this the
/// schedule would silently fall back to the path default — the exact failure the field exists to
/// prevent, and one that looks like the agent simply running out of turns.
#[test]
fn a_schedules_max_turns_reaches_the_goal_spec() {
    use crate::helpers::reaction_goal;
    use liberado_common::{Event, EventPayload};

    let with = |data: serde_json::Value| {
        Event::trigger(
            "CronFired",
            "cron:bigjob",
            "c1",
            EventPayload {
                data,
                ..Default::default()
            },
        )
    };

    assert_eq!(
        reaction_goal(
            &with(serde_json::json!({"max_turns": 20})),
            "do it",
            "default"
        )
        .max_turns,
        20
    );

    // Absent, null, and a non-number all mean "pack default" (0) — the behaviour every schedule
    // had before the field existed. Anything else would change existing deployments silently.
    for payload in [
        serde_json::json!({}),
        serde_json::Value::Null,
        serde_json::json!({"max_turns": "20"}),
    ] {
        assert_eq!(
            reaction_goal(&with(payload), "do it", "default").max_turns,
            0
        );
    }

    // Coexists with the other payload riders.
    assert_eq!(
        reaction_goal(
            &with(serde_json::json!({"profile": "hat", "deliver": false, "max_turns": 12})),
            "do it",
            "default"
        )
        .max_turns,
        12
    );
}

#[test]
fn cron_delivery_is_suppressed_only_by_an_explicit_false() {
    use crate::helpers::cron_delivery_suppressed;
    use liberado_common::{Event, EventPayload};

    let with = |data: serde_json::Value| {
        Event::trigger(
            "CronFired",
            "cron:sweep",
            "c1",
            EventPayload {
                data,
                ..Default::default()
            },
        )
    };

    assert!(cron_delivery_suppressed(&with(
        serde_json::json!({"deliver": false})
    )));

    // Everything else delivers. `Null` and a missing key are what every event looked like before
    // the flag existed, so treating either as "suppress" would silence the whole system.
    assert!(!cron_delivery_suppressed(&with(
        serde_json::json!({"deliver": true})
    )));
    assert!(!cron_delivery_suppressed(&with(serde_json::json!({}))));
    assert!(!cron_delivery_suppressed(&with(serde_json::Value::Null)));
    // A non-bool is a config mistake; delivering is the safe reading of an unclear answer.
    assert!(!cron_delivery_suppressed(&with(
        serde_json::json!({"deliver": "false"})
    )));
    // Coexists with profile, which shares the payload map.
    assert!(cron_delivery_suppressed(&with(
        serde_json::json!({"profile": "hat", "deliver": false})
    )));
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
    // F12 scopes the watcher to `inbox/`. Create it before any test starts the
    // watch: Linux inotify does not reliably deliver events for a directory
    // created after the watch is armed. Windows CI still passed without this.
    std::fs::create_dir_all(dir.path().join("inbox")).unwrap();
    let daemon = Daemon::open("test", dir.path())
        .await
        .unwrap()
        // Approval authority lives outside the vault, so a fixture that executes proposals needs
        // one attached — rooted inside the same temp dir so it dies with the test. A test that
        // *approves* something calls `approve_in` for the matching decision; one that does not is
        // asserting the refusal, which is the default.
        .with_approval_ledger(test_ledger(&dir));
    (daemon, dir)
}

/// The ledger `temp_daemon` attaches, addressable from a test that needs to record a decision.
fn test_ledger(dir: &TempDir) -> liberado_common::ApprovalLedger {
    liberado_common::ApprovalLedger::new(dir.path().join(".approvals"))
}

/// Record the human approval a proposal needs before the daemon will run it — the ledger entry a
/// Telegram tap would create. Without this the note's `status: approved` is only a claim.
async fn approve_in(dir: &TempDir, proposal_id: &str) {
    test_ledger(dir)
        .record(
            proposal_id,
            liberado_common::ApprovalDecision::Approved,
            "test",
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn external_change_produces_reaction() {
    let (daemon, dir) = temp_daemon().await;
    // A human writes a note directly into the capture path (not through the adapter) —
    // no matching audit entry.
    std::fs::create_dir_all(dir.path().join("inbox")).unwrap();
    std::fs::write(dir.path().join("inbox/note.md"), "a human wrote this").unwrap();

    let event = daemon
        .process_change(Path::new("inbox/note.md"))
        .await
        .unwrap()
        .expect("external change should produce a reaction");
    assert_eq!(event.event_type, VAULT_NOTE_CHANGED);
    assert_eq!(event.source, event_source::TURBOVAULT_SUBSCRIPTION);
    assert_eq!(event.payload.path.as_deref(), Some("inbox/note.md"));
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

    // Give the watcher a moment to establish before writing into the capture path.
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::create_dir_all(vault_dir.join("inbox")).unwrap();
    std::fs::write(
        vault_dir.join("inbox/captured.md"),
        "dropped in from Obsidian",
    )
    .unwrap();

    // Exactly one reaction, despite notify firing Create + Modify + ... for one write.
    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");
    assert_eq!(
        reaction.event.payload.path.as_deref(),
        Some("inbox/captured.md")
    );
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
            delivery: Delivery::Summarize,
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
            delivery: Delivery::Summarize,
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
    std::fs::create_dir_all(vault_dir.join("inbox")).unwrap();
    std::fs::write(vault_dir.join("inbox/email-me.md"), "please email the boss").unwrap();

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
    // (`docs/future-work/archive/hygiene-audit-2026-07-05.md` P3.4). The MCP's own `consequence` is
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
            delivery: Delivery::Summarize,
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
    std::fs::create_dir_all(vault_dir.join("inbox")).unwrap();
    std::fs::write(
        vault_dir.join("inbox/review-me.md"),
        "please file this review",
    )
    .unwrap();

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
    // The note claims approval; this is the human decision that makes it real.
    approve_in(&dir, "vault-change:test-proposal:abc").await;
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
    )
    .with_requested_grant(liberado_common::Capability::Write(
        liberado_common::Zone::vault("tasks"),
    ));
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

/// Full lifecycle through the hub path: vault watch → handle_proposal_change → execute
/// → archive → grant. Unlike `daemon_executes_an_approved_proposal` (which uses the direct
/// orchestrator path), this test attaches a hub so the session grant is applied.
#[tokio::test]
async fn daemon_hub_proposal_lifecycle_applies_grant() {
    use liberado_common::{
        DEFAULT_POOL, GrantScope, Proposal, ProposalSigner, ProposalStatus, ProposedAction,
        ToolCall, session_grants,
    };
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
    use liberado_session::{GoalSessionHub, GoalSessionStore};
    use std::sync::Arc;

    struct LpRuntime;
    #[async_trait::async_trait]
    impl ToolRuntime for LpRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    struct LpFactory;
    #[async_trait::async_trait]
    impl RuntimeFactory for LpFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: liberado_common::WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(LpRuntime))
        }
    }

    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "proposal lifecycle" }),
            )])],
        )),
        LpFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    );

    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(liberado_session::LifeOpsDemoRunner));
    let hub = Arc::new(hub);

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone())
        .with_goal_hub(hub.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    // The note claims approval; this is the human decision that makes it real.
    approve_in(&dir, "vault-change:lifecycle:1").await;

    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut proposal = Proposal::pending(
        "vault-change:lifecycle:1",
        "vault-change:lifecycle:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "lifecycle test" }),
        }]),
        "proposal lifecycle test",
    )
    .with_requested_grant(liberado_common::Capability::Write(
        liberado_common::Zone::vault("lifecycle"),
    ));
    proposal.pool = Some(DEFAULT_POOL.to_string());
    proposal.approved_scope = Some(GrantScope::Session);
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("lifecycle.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    // Grant applied.
    let grant = session_grants::session_grant(DEFAULT_POOL);
    assert!(
        !grant.capabilities.is_empty(),
        "hub lifecycle: grant must be non-empty, got {grant:?}"
    );
    assert!(
        grant.contains(&liberado_common::Capability::Write(
            liberado_common::Zone::vault("lifecycle")
        )),
        "hub lifecycle: grant must include Write(vault/\"lifecycle\"): {grant:?}"
    );

    // Proposal archived.
    let archived = proposals_dir.join("archive/approved/lifecycle.md");
    let mut found = false;
    for _ in 0..50 {
        if archived.exists() {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(found, "proposal must be archived at {archived:?}");
    assert!(
        !proposals_dir.join("lifecycle.md").exists(),
        "archived proposal must be removed from active proposals dir"
    );
    let archived_proposal =
        Proposal::from_note(&std::fs::read_to_string(&archived).unwrap()).unwrap();
    assert_eq!(archived_proposal.status, ProposalStatus::Done);

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

/// Free-form Failed summaries that merely mention "expired" must not be treated as the
/// orchestrator's preflight refuse signal (exact `EXPIRED_PROPOSAL_REFUSAL_SUMMARY` only).
#[test]
fn expired_refusal_matches_exact_orchestrator_summary_only() {
    use liberado_orchestrator::EXPIRED_PROPOSAL_REFUSAL_SUMMARY;
    assert_eq!(
        EXPIRED_PROPOSAL_REFUSAL_SUMMARY,
        "proposal expired — not executed"
    );
    // Substring traps would mis-handle free-form executor text:
    assert!(
        "subagent said the lease expired mid-run".contains("expired")
            && "subagent said the lease expired mid-run" != EXPIRED_PROPOSAL_REFUSAL_SUMMARY
    );
}

#[tokio::test]
async fn handle_proposal_change_expired_refuse_does_not_apply_session_grant() {
    // Permission request past deadline: execute_approved refuses without tools — must not
    // persist a Session grant (apply_approved_grant only after a real execute path).
    use chrono::{Duration as ChronoDuration, Utc};
    use liberado_common::{
        Capability, CapabilitySet, GrantScope, Proposal, ProposalSigner, ProposalStatus,
        ProposedAction, ToolCall, WriteProvenance, session_grants,
    };
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::MockProvider;
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};
    use std::path::Path;
    use std::sync::Arc;

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

    let pool = format!("grant-expire-test-{:?}", std::thread::current().id());
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let mut proposal = Proposal::pending(
        "vault-change:expired-grant:1",
        "vault-change:expired-grant:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "no" }),
        }]),
        "permission request past deadline",
    )
    .with_requested_grant(Capability::Write(liberado_common::Zone::vault("sandbox")));
    proposal.pool = Some(pool.clone());
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(1));
    // Session scope is part of the signed payload (set before sign).
    proposal.approved_scope = Some(GrantScope::Session);
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);

    let rel = Path::new("proposals/expired-grant.md");
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
        "tools must not run on expired refuse"
    );
    assert!(
        session_grants::session_grant(&pool).capabilities.is_empty(),
        "session grant must not persist when execute was refused as expired"
    );
    // Lifecycle: expired + archived
    assert!(daemon.vault.read(rel).await.is_err());
    let archived = daemon
        .vault
        .read("proposals/archive/expired/expired-grant.md")
        .await
        .expect("must archive as expired");
    assert_eq!(
        Proposal::from_note(&archived).unwrap().status,
        ProposalStatus::Expired
    );
}

/// `complete_refusal_lifecycle` must archive a proposal as `Expired` *only* when the
/// orchestrator returns the exact `EXPIRED_PROPOSAL_REFUSAL_SUMMARY` (a pre-execution expired
/// refusal where tools never ran). Any other report — Failed with a different summary, or a
/// Succeeded report that merely mentions expiry — must leave the note untouched. A mutant that
/// drops the body (`None`), flips the equality (`!=`→`==`), or flips the operator (`||`→`&&`)
/// all change which reports get archived; the three cases below pin each branch.
#[tokio::test]
async fn complete_refusal_lifecycle_archives_only_on_exact_expired_refusal() {
    use liberado_common::{Outcome, Report, WriteProvenance};
    use liberado_orchestrator::EXPIRED_PROPOSAL_REFUSAL_SUMMARY;

    let (daemon, dir) = temp_daemon().await;
    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let prov = WriteProvenance::agent("test", "c1");

    fn refusal_report(outcome: Outcome, summary: &str) -> Report {
        Report {
            outcome,
            summary: summary.to_string(),
            artifacts: Vec::new(),
            new_high_signal_facts: Vec::new(),
            deferred_to_human: false,
            follow_up: None,
            repeat_calls: 0,
        }
    }

    // One case per proposal id so archives never bleed across assertions.
    async fn case(
        daemon: &crate::Daemon,
        prov: &WriteProvenance,
        id: &str,
        outcome: Outcome,
        summary: &str,
        expect_archive_as_expired: bool,
    ) {
        use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
        use std::path::Path;
        let base = Proposal::pending(
            id,
            id,
            "test",
            ProposedAction::ToolCalls(vec![ToolCall {
                tool: "tasks:create".into(),
                args: serde_json::json!({ "summary": "x" }),
            }]),
            "a proposal",
        );
        let mut signed = ProposalSigner::random().sign(base);
        signed.set_status(ProposalStatus::Approved);
        let mut proposal = signed.into_proposal();

        let rel = Path::new("proposals").join(format!("{id}.md"));
        daemon
            .vault
            .write(&rel, &proposal.to_note(), None, prov)
            .await
            .unwrap();
        let archive_rel = format!("proposals/archive/expired/{id}.md");

        let result = daemon
            .complete_refusal_lifecycle(&rel, &mut proposal, &refusal_report(outcome, summary))
            .await;

        if expect_archive_as_expired {
            assert!(
                matches!(result, Some(crate::ReactionOutcome::Observed)),
                "{id}: exact expired refusal must complete the lifecycle"
            );
            assert!(
                daemon.vault.read(&rel).await.is_err(),
                "{id}: original must be moved"
            );
            let archived = daemon
                .vault
                .read(&archive_rel)
                .await
                .unwrap_or_else(|_| panic!("{id}: must archive as expired"));
            assert_eq!(
                Proposal::from_note(&archived).unwrap().status,
                ProposalStatus::Expired,
                "{id}: archived proposal must carry Expired status"
            );
        } else {
            assert!(
                result.is_none(),
                "{id}: report must not complete the expiry lifecycle"
            );
            assert!(
                daemon.vault.read(&rel).await.is_ok(),
                "{id}: note must be left in place"
            );
            assert!(
                daemon.vault.read(&archive_rel).await.is_err(),
                "{id}: must not archive"
            );
        }
    }

    // Case A: exact expired refusal → archive as Expired.
    case(
        &daemon,
        &prov,
        "expire-lifecycle-a",
        Outcome::Failed,
        EXPIRED_PROPOSAL_REFUSAL_SUMMARY,
        true,
    )
    .await;

    // Case B: Failed with a *different* summary → leave the note in place, no archive.
    // Pins the `summary != EXPIRED` half of the condition (catches `!=`→`==` on the summary).
    case(
        &daemon,
        &prov,
        "expire-lifecycle-b",
        Outcome::Failed,
        "some other failure",
        false,
    )
    .await;

    // Case C: Succeeded report whose summary happens to match → must NOT archive.
    // Pins the `outcome != Failed` half of the condition (catches `||`→`&&`).
    case(
        &daemon,
        &prov,
        "expire-lifecycle-c",
        Outcome::Succeeded,
        EXPIRED_PROPOSAL_REFUSAL_SUMMARY,
        false,
    )
    .await;
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

/// An approved **subagent** proposal runs inference, and that inference must be attributed.
///
/// Boundary under test (R3): the wrap lives in `Daemon::handle_proposal_change`, and the only way
/// to see whether it worked is what `MeteredProvider` handed the recorder — so the assertion is on
/// captured `LatencyEvent.correlation`, not on a function having been called.
///
/// Also the approval path's role boundary (deliverable §2): the approved-subagent arm (1205) runs
/// on the subagent-tagged provider, so every event it records must carry `role: "subagent"` — an
/// implementation that still routes approval-path work through the orchestrator's own provider
/// (labelling it `orchestrator`, indistinguishable from `ExecuteDirect`) fails here.
///
/// R1: removing the `with_correlation` wrap around `execute_approved` in `proposals.rs` fails this
/// with `left: "-"`. That was the live behaviour: 14 of the deployed journal's 104 unattributed
/// calls came through this path, and they are the expensive kind — agent loops reaching 29k prompt
/// tokens with nothing to charge them to.
#[tokio::test]
async fn approved_subagent_execution_is_attributed_to_the_proposal_correlation() {
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::{
        AgentRole, CompletionResponse, LatencyEvent, LatencyRecorder, MeteredProvider,
        MockProvider, Provider,
    };
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

    #[derive(Default)]
    struct CapturingRecorder {
        events: std::sync::Mutex<Vec<LatencyEvent>>,
    }
    impl LatencyRecorder for CapturingRecorder {
        fn record(&self, event: LatencyEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    const CORRELATION: &str = "vault-change:Learning/Attribution.md:deadbeef";

    let rec = Arc::new(CapturingRecorder::default());
    // A subagent loop nudges for a `submit_report`; script enough plain replies that the executor's
    // own budget ends the run rather than the mock running dry. Whether the subagent *succeeds* is
    // not what this test is about — only that whatever it spent is attributable.
    let inner: Arc<dyn Provider> = Arc::new(MockProvider::with_script(
        "mock",
        (0..24).map(|i| CompletionResponse::text(format!("step {i}"))),
    ));
    // Two metered instances over the same backend: the orchestrator's own provider (tagged
    // `Orchestrator`, for direct execution) and the subagent-tagged one the approval path runs on.
    let metered_orchestrator = MeteredProvider::wrap(
        inner.clone(),
        AgentRole::Orchestrator,
        rec.clone() as Arc<dyn LatencyRecorder>,
    );
    let metered_subagent = MeteredProvider::wrap(
        inner,
        AgentRole::Subagent,
        rec.clone() as Arc<dyn LatencyRecorder>,
    );

    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        metered_orchestrator,
        InvocationRecordingFactory {
            runtime: InvocationRecordingRuntime::default(),
        },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        signer.clone(),
        "default",
    )
    .with_subagent_provider(metered_subagent);

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());
    std::fs::create_dir_all(dir.path().join("proposals")).unwrap();
    // The ledger entry a human's Telegram tap would create. Without it the note's `status:
    // approved` is only a claim and execution is refused — which would make this test pass
    // vacuously if the precondition below were not asserted.
    approve_in(&dir, CORRELATION).await;

    let proposal = Proposal::pending(
        CORRELATION,
        CORRELATION,
        "test",
        ProposedAction::Subagent {
            goal: "summarise the note".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: Vec::new(),
            success_criteria: Vec::new(),
        },
        "an approved subagent",
    );
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    let rel = Path::new("proposals/attributed.md");
    let prov = WriteProvenance::agent("test", CORRELATION);
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &prov)
        .await
        .unwrap();

    daemon.handle_proposal_change(rel).await.unwrap();

    let events = rec.events.lock().unwrap();
    assert!(
        !events.is_empty(),
        "an approved subagent must reach the provider — otherwise this test proves nothing"
    );
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(
            ev.correlation, CORRELATION,
            "event[{i}] must carry the proposal's correlation, not \"-\""
        );
        assert_eq!(
            ev.role, "subagent",
            "event[{i}] is delegated subagent work and must not merge into the orchestrator role"
        );
    }
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

/// A proposal that the human has *legitimately approved* via the ledger, but whose integrity
/// signature was forged (or tampered with after signing), must STILL be refused. Without this
/// assertion, a mutant that replaces `reject_if_tampered` with `None` would let a forged
/// proposal slide through every later guard (terminal, expiry, actionable, ledger) and run.
///
/// `daemon_rejects_an_approved_proposal_with_a_bad_integrity_signature` (above) covers the
/// "no ledger approval" variant — the ledger guard there refuses before execution gets a
/// chance. Here the ledger DOES approve, so the only thing standing between a forged
/// proposal and a tool call is the daemon's integrity check. To prove the daemon's check is
/// the one that fires (not the orchestrator's), the test uses *two different signers*:
/// the orchestrator's signer verifies the proposal, the daemon's signer would not.
#[tokio::test]
async fn forged_proposal_with_a_ledger_approval_still_does_not_execute() {
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
    use liberado_orchestrator::Orchestrator;
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    // The orchestrator's signer (used to defend-in-depth-verify the proposal just before
    // `runtime.invoke`). The daemon's signer is *different*, so only the daemon's check can
    // catch the forgery.
    let orch_signer = ProposalSigner::random();
    let daemon_signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(liberado_provider::MockProvider::with_script(
            "mock",
            Vec::<liberado_provider::CompletionResponse>::new(),
        )),
        InvocationRecordingFactory { runtime },
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        orch_signer.clone(),
        "default",
    );

    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        // The daemon verifies the proposal against `daemon_signer`, not the orchestrator's
        // `orch_signer`. A proposal signed by the orchestrator's key is fine for the
        // orchestrator's defense-in-depth check, but the daemon's check still fires.
        .with_proposal_signer(daemon_signer)
        .with_approval_ledger(test_ledger(&dir));

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    // The human's approval is real — it lands in the ledger. A mutant bypassing only the
    // integrity check still has to clear this guard.
    approve_in(&dir, "forged-but-approved:1").await;

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Sign the proposal with the orchestrator's key (so the orchestrator's signer.verify
    // returns true) but NOT the daemon's (so the daemon's check is the one that must fire).
    let proposal = Proposal::pending(
        "forged-but-approved:1",
        "forged-but-approved:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should-not-run" }),
        }]),
        "a forged but ledger-approved proposal",
    )
    .with_requested_grant(liberado_common::Capability::Write(
        liberado_common::Zone::vault("tasks"),
    ));
    let mut proposal = orch_signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("forged-approved.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    assert!(
        invoked.lock().unwrap().is_empty(),
        "a forged proposal must not run even when the ledger says Approved; \
         a mutant that drops reject_if_tampered would let the tool call through"
    );

    handle.abort();
}

/// Without an approval ledger attached, even a *fully legitimate* proposal must be refused
/// silently. The note's `status: approved` is just a claim — the ledger is the authority.
/// A mutant that replaces `refuse_without_ledger_approval` with `None` would let the note's
/// claim be enough, and the orchestrator would run the tool.
#[tokio::test]
async fn approved_proposal_without_a_ledger_does_not_execute() {
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
    use liberado_orchestrator::Orchestrator;
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    // Same signer for both daemon and orchestrator, so neither integrity check fires — the
    // ledger check is the only one that can stop this.
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(liberado_provider::MockProvider::with_script(
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
    // NO `with_approval_ledger` call — `self.approvals` is None. The proposal claims Approved
    // and is properly signed, so the only thing that can stop the tool is the ledger check.
    let daemon = daemon
        .with_debounce(Duration::from_millis(80))
        .with_orchestrator(orch)
        .with_proposal_signer(signer.clone());

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let proposal = Proposal::pending(
        "no-ledger:1",
        "no-ledger:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should-not-run" }),
        }]),
        "an approved but un-ledgered proposal",
    )
    .with_requested_grant(liberado_common::Capability::Write(
        liberado_common::Zone::vault("tasks"),
    ));
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("no-ledger.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    assert!(
        invoked.lock().unwrap().is_empty(),
        "a proposal claiming Approved without a ledger entry must not execute; \
         a mutant that drops refuse_without_ledger_approval would let it through"
    );

    handle.abort();
}

/// A proposal whose ledger entry is *explicitly* Rejected must not execute — even with a
/// matching signature and `status: approved` in the note. The ledger overrides the note.
#[tokio::test]
async fn rejected_proposal_in_ledger_does_not_execute() {
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
    use liberado_orchestrator::Orchestrator;
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};

    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let signer = ProposalSigner::random();
    let orch = Orchestrator::new(
        Arc::new(liberado_provider::MockProvider::with_script(
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
        .with_proposal_signer(signer.clone())
        .with_approval_ledger(test_ledger(&dir));

    let proposals_dir = dir.path().join("proposals");
    std::fs::create_dir_all(&proposals_dir).unwrap();

    // The human *rejected* this proposal out of band. The note's status:approved is a stale
    // claim that should be overruled by the ledger.
    test_ledger(&dir)
        .record(
            "rejected-by-human:1",
            liberado_common::ApprovalDecision::Rejected,
            "test",
        )
        .await
        .unwrap();

    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));

    tokio::time::sleep(Duration::from_millis(300)).await;

    let proposal = Proposal::pending(
        "rejected-by-human:1",
        "rejected-by-human:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "should-not-run" }),
        }]),
        "an approved note but rejected by human",
    )
    .with_requested_grant(liberado_common::Capability::Write(
        liberado_common::Zone::vault("tasks"),
    ));
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    std::fs::write(proposals_dir.join("rejected.md"), proposal.to_note()).unwrap();

    let _reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for reaction")
        .expect("reaction channel closed");

    assert!(
        invoked.lock().unwrap().is_empty(),
        "a proposal whose ledger says Rejected must not execute even if the note says Approved; \
         a mutant that drops refuse_without_ledger_approval would let it through"
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

    // The human approves. Two things now, and the order is the point: the decision is recorded in
    // the ledger — outside the vault, where no tool reaches — and the note is updated as the
    // human-readable view of it. Flipping the note alone would authorise nothing, which is the
    // whole reason the ledger exists.
    approve_in(&dir, &written.id).await;
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
            delivery: Delivery::Summarize,
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
    // The grant must carry the profile's configured capabilities (the whole point of the
    // profile lookup). Without this assertion, a mutant that drops the `capabilities:` field
    // from the SessionGrant struct literal here would silently downgrade the session to a
    // default-empty grant and run with no authority at all.
    assert!(
        snap.session.grant.capabilities.grants_ask_human(),
        "the session grant must inherit the profile's AskHuman capability: got {:?}",
        snap.session.grant.capabilities
    );
    assert_eq!(
        snap.session.grant.profile.as_deref(),
        Some("interactive-cron"),
        "the grant's profile field must echo the resolved profile"
    );
    assert!(
        snap.session.grant.overrides.is_null(),
        "no profile-supplied overrides were configured; the grant's overrides must be Value::Null"
    );

    handle.abort();
}

// ── L9 — cron/webhook-class event → joinable Dispatched session (T1 suite) ──

/// L9 (docs/impl/live-conformance.md): a cron/webhook-class event on the **shipped**
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
            delivery: Delivery::Summarize,
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

#[tokio::test]
async fn handle_proposal_change_active_failed_not_expired_does_not_enter_expiry_path() {
    use chrono::{Duration as ChronoDuration, Utc};
    use liberado_common::{
        Capability, DEFAULT_POOL, GrantScope, Proposal, ProposalSigner, ProposalStatus,
        ProposedAction, ToolCall, WriteProvenance, session_grants,
    };
    use liberado_executor::SUBMIT_REPORT_TOOL;
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
    use liberado_test_support::{InvocationRecordingFactory, InvocationRecordingRuntime};
    use std::path::Path;
    use std::sync::Arc;

    let signer = ProposalSigner::random();
    let runtime = InvocationRecordingRuntime::default();
    let invoked = runtime.invoked.clone();
    let orch = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c1",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "failed", "summary": "something went wrong" }),
            )])],
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

    // Proposal is NOT expired (future deadline) — must pass step 4 in handle_proposal_change
    // and reach the orchestrator. The orchestrator returns Failed but with a generic summary
    // that does NOT match the expiry refusal summary (lines 184-185).
    let mut proposal = Proposal::pending(
        "vault-change:gen-fail:1",
        "vault-change:gen-fail:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "test" }),
        }]),
        "generic failure test",
    )
    .with_requested_grant(Capability::Write(liberado_common::Zone::vault("sandbox")));
    proposal.pool = Some(DEFAULT_POOL.to_string());
    proposal.expires = Some(Utc::now() + ChronoDuration::hours(1));
    proposal.approved_scope = Some(GrantScope::Session);
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);

    let rel = Path::new("proposals/gen-fail.md");
    let prov = WriteProvenance::agent("test", "c1");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &prov)
        .await
        .unwrap();

    // The note claims approval; this is the human decision that makes it real.
    approve_in(&dir, "vault-change:gen-fail:1").await;

    let _outcome = daemon.handle_proposal_change(rel).await.unwrap();
    assert!(
        !invoked.lock().unwrap().is_empty(),
        "tools should have run — the orchestrator returned Failed but not with the expiry refusal"
    );
    let grant = session_grants::session_grant(DEFAULT_POOL);
    assert!(
        !grant.capabilities.is_empty(),
        "session grant must be applied when the failed outcome is not the expiry refusal"
    );
}

/// L9 extended: a webhook event (not cron) injected via the daemon's event_sender produces a
/// joinable, terminal background session — proving the event→daemon→hub→session path is
/// source-agnostic (webhook and cron share the same dispatch pipeline).
#[tokio::test]
async fn l9_webhook_event_becomes_joinable_dispatched_session() {
    use liberado_common::{DispatchAction, DispatchDecision};
    use liberado_config_loader::DispatchTuning;
    use liberado_dispatch_pack::{DISPATCH_DOMAIN, DispatchPack};
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
    use liberado_session::{
        GoalSessionHub, GoalSessionStore, SessionStatus, TerminalKind, Visibility,
    };
    use std::sync::Arc;

    struct WebhookL9Runtime;
    #[async_trait::async_trait]
    impl ToolRuntime for WebhookL9Runtime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    struct WebhookL9Factory;
    #[async_trait::async_trait]
    impl RuntimeFactory for WebhookL9Factory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(WebhookL9Runtime))
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
        rationale: "webhook task".into(),
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
                serde_json::json!({ "outcome": "succeeded", "summary": "webhook task done" }),
            )])],
        )),
        WebhookL9Factory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
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
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Webhook event — same Event::trigger shape the production POST /api/hooks/{name} handler
    // produces, but injected here to decouple the test from the HTTP layer (which is covered
    // by the hooks.rs integration tests).
    sender
        .send(Event::trigger(
            "WebhookFired",
            "webhook:nightly-backup",
            "webhook:nightly-backup:t1",
            liberado_common::EventPayload {
                summary: Some("back up the vault".into()),
                ..Default::default()
            },
        ))
        .unwrap();

    let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for webhook reaction")
        .expect("reaction channel closed");
    assert_eq!(reaction.event.source, "webhook:nightly-backup");

    let session_id = match reaction.outcome {
        ReactionOutcome::Dispatched { session_id } => session_id,
        other => panic!("expected Dispatched for webhook, got {}", other.label()),
    };

    let snap = hub.await_terminal(&session_id).await.expect("terminal");
    let row = &snap.session;

    assert_eq!(row.visibility, Visibility::Background);
    assert_eq!(row.goal.description, "back up the vault");
    assert_eq!(row.status, SessionStatus::Succeeded);
    assert_eq!(
        row.result.as_ref().unwrap().terminal,
        TerminalKind::Succeeded
    );
    assert!(
        row.result.as_ref().unwrap().summary.contains("webhook"),
        "session summary must contain the webhook work: {:?}",
        row.result.as_ref().unwrap().summary
    );
    assert_eq!(
        row.goal.domain.as_str(),
        DISPATCH_DOMAIN,
        "webhook reaction sessions run under the dispatch domain"
    );

    handle.abort();
}

/// L9 extended: notifier delivery confirmation — a webhook-triggered session calls
/// `Notifier::deliver_cron` with the session's terminal summary once it completes.
#[tokio::test]
async fn l9_webhook_session_triggers_notifier_deliver_cron() {
    use liberado_common::{DispatchAction, DispatchDecision};
    use liberado_config_loader::DispatchTuning;
    use liberado_dispatch_pack::DispatchPack;
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
    use liberado_notify::Notifier;
    use liberado_orchestrator::Orchestrator;
    use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
    use liberado_session::{GoalSessionHub, GoalSessionStore, SessionStatus};
    use std::sync::Arc;

    struct L9NotifyRuntime;
    #[async_trait::async_trait]
    impl ToolRuntime for L9NotifyRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    struct L9NotifyFactory;
    #[async_trait::async_trait]
    impl RuntimeFactory for L9NotifyFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(L9NotifyRuntime))
        }
    }

    #[derive(Default)]
    struct RecordingNotifier {
        calls: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl Notifier for RecordingNotifier {
        async fn notify(&self, _message: &str) -> Result<(), liberado_notify::NotifyError> {
            Ok(())
        }
        async fn deliver_cron(&self, message: &str) -> Result<(), liberado_notify::NotifyError> {
            self.calls.lock().unwrap().push(message.to_string());
            Ok(())
        }
    }

    let recorded_calls: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let notifier = Arc::new(RecordingNotifier {
        calls: recorded_calls.clone(),
    });

    let (daemon, _dir) = temp_daemon().await;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "notify task".into(),
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
                serde_json::json!({ "outcome": "succeeded", "summary": "notified task done" }),
            )])],
        )),
        L9NotifyFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
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
        .with_goal_hub(hub.clone())
        .with_notifier(notifier);

    let sender = daemon.event_sender();
    let (tx, mut rx) = unbounded_channel();
    let handle = tokio::spawn(daemon.run(tx));
    tokio::time::sleep(Duration::from_millis(50)).await;

    sender
        .send(Event::trigger(
            "CronFired",
            "cron:notify-test",
            "cron:notify-test:t1",
            liberado_common::EventPayload {
                summary: Some("verify notifier delivery".into()),
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
        other => panic!("expected Dispatched, got {}", other.label()),
    };

    let snap = hub.await_terminal(&session_id).await.expect("terminal");
    assert_eq!(
        snap.session.status,
        SessionStatus::Succeeded,
        "notifier-test session should succeed, got {:?}: result={:?}",
        snap.session.status,
        snap.session.result
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    let recorded = recorded_calls.lock().unwrap();
    assert_eq!(
        recorded.len(),
        1,
        "deliver_cron must be called exactly once, got {:?}",
        *recorded
    );
    assert!(
        recorded[0].contains("notify-test"),
        "notifier message must contain the schedule name, got: {:?}",
        recorded[0]
    );

    handle.abort();
}

// ════════════════════════════════════════════════════════════════════════════
// Phase 5: Guard Conformance — dispatcher pre-flight ↔ runtime enforcement
// ════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn guard_conformance_capability_gap_agrees_both_sides() {
    use liberado_common::{
        BlockReason, Capability, CapabilitySet, Consequence, DispatchAction, DispatchDecision,
        McpDescriptor, ToolCall,
    };
    use liberado_config_loader::DispatchTuning;
    use liberado_dispatcher::guards::evaluate;
    use liberado_executor::{RiskGatedToolRuntime, ToolRuntime};
    use liberado_provider::{ToolDef, ToolInvocation};
    use std::sync::Arc;

    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]);
    let catalog = vec![McpDescriptor {
        name: "email-mcp".into(),
        description: "email".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    }];

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "email-mcp:send".into(),
                args: serde_json::json!({}),
            }],
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let req = liberado_dispatcher::DispatchRequest {
        goal: "send an email".into(),
        catalog,
        capabilities: caps.clone(),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
    };
    assert_eq!(
        evaluate(&decision, &req, &DispatchTuning::default(), 4),
        Some(BlockReason::CapabilityGap),
        "dispatcher: ungranted MCP must be CapabilityGap"
    );

    struct NoopRt;
    #[async_trait::async_trait]
    impl ToolRuntime for NoopRt {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    let rt = RiskGatedToolRuntime::new(
        Arc::new(NoopRt),
        caps,
        vec![("email-mcp".into(), Consequence::Reversible)],
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        "send an email".into(),
        "t1-guards-cap".into(),
        ProposalSigner::random(),
        "default",
    );
    let runtime_result = rt
        .invoke(&ToolInvocation::new(
            "c1",
            "email-mcp:send",
            serde_json::json!({}),
        ))
        .await;
    assert!(
        runtime_result.is_err(),
        "runtime must also reject ungranted MCP"
    );
}

#[tokio::test]
async fn guard_conformance_consequence_agrees_on_external_mcp() {
    use liberado_common::{
        BlockReason, Capability, CapabilitySet, Consequence, DispatchAction, DispatchDecision,
        McpDescriptor, ToolCall,
    };
    use liberado_config_loader::DispatchTuning;
    use liberado_dispatcher::guards::evaluate;
    use liberado_executor::{RiskGatedToolRuntime, ToolRuntime};
    use liberado_provider::{ToolDef, ToolInvocation};
    use std::sync::Arc;

    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("email".into())]);
    let catalog = vec![McpDescriptor {
        name: "email".into(),
        description: "send email".into(),
        consequence: Consequence::External,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    }];

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "email:send".into(),
                args: serde_json::json!({}),
            }],
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let req = liberado_dispatcher::DispatchRequest {
        goal: "email my boss".into(),
        catalog,
        capabilities: caps.clone(),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
    };
    assert_eq!(
        evaluate(&decision, &req, &DispatchTuning::default(), 4),
        Some(BlockReason::HighConsequence),
        "dispatcher: External MCP must be HighConsequence"
    );

    struct NoopRt2;
    #[async_trait::async_trait]
    impl ToolRuntime for NoopRt2 {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    let rt = RiskGatedToolRuntime::new(
        Arc::new(NoopRt2),
        caps,
        vec![("email".into(), Consequence::External)],
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        "email my boss".into(),
        "t1-guards-consequence".into(),
        ProposalSigner::random(),
        "default",
    );
    let runtime_result = rt
        .invoke(&ToolInvocation::new(
            "c1",
            "email:send",
            serde_json::json!({}),
        ))
        .await;
    assert!(
        runtime_result.is_ok() && runtime_result.unwrap().contains("PROPOSAL"),
        "runtime must downgrade External MCP to a proposal"
    );
}

#[tokio::test]
async fn guard_conformance_magnitude_agrees_on_sweeping_destructive() {
    use liberado_common::{
        BlockReason, Capability, CapabilitySet, Consequence, DispatchAction, DispatchDecision,
        McpDescriptor, ToolCall,
    };
    use liberado_config_loader::DispatchTuning;
    use liberado_dispatcher::guards::evaluate;
    use liberado_executor::{RiskGatedToolRuntime, ToolRuntime};
    use liberado_provider::{ToolDef, ToolInvocation};
    use std::sync::Arc;

    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]);
    let catalog = vec![McpDescriptor {
        name: "vault".into(),
        description: "git-tracked vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    }];
    let goal = "delete all of my notes and erase everything";

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: vec![ToolCall {
                tool: "vault:write".into(),
                args: serde_json::json!({}),
            }],
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "test".into(),
    };
    let req = liberado_dispatcher::DispatchRequest {
        goal: goal.into(),
        catalog,
        capabilities: caps.clone(),
        reaction_depth: 0,
        zone_write_classes: Vec::new(),
    };
    assert_eq!(
        evaluate(&decision, &req, &DispatchTuning::default(), 4),
        Some(BlockReason::HighConsequence),
        "dispatcher: sweeping-destructive goal must be HighConsequence"
    );

    struct NoopRt3;
    #[async_trait::async_trait]
    impl ToolRuntime for NoopRt3 {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Ok("ok".into())
        }
    }
    let rt = RiskGatedToolRuntime::new(
        Arc::new(NoopRt3),
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        goal.into(),
        "t1-guards-magnitude".into(),
        ProposalSigner::random(),
        "default",
    );
    let runtime_result = rt
        .invoke(&ToolInvocation::new(
            "c1",
            "vault:write",
            serde_json::json!({}),
        ))
        .await;
    assert!(
        runtime_result.is_ok() && runtime_result.unwrap().contains("PROPOSAL"),
        "runtime must downgrade sweeping-destructive call to a proposal"
    );
}

#[tokio::test]
async fn concurrent_park_and_cancel_do_not_deadlock() {
    use liberado_session::{
        DomainHint, DomainPackRunner, GoalResult, GoalSessionHub, GoalSessionStore, GoalSpec,
        InputChannel, PackContext, PackError, SessionEvent, SessionGrant, SessionStatus,
    };
    use std::sync::Arc;

    struct ConcurrentSpyPack {
        pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait::async_trait]
    impl DomainPackRunner for ConcurrentSpyPack {
        fn domain_id(&self) -> &str {
            "life"
        }
        async fn run(
            &self,
            _id: &str,
            _goal: &GoalSpec,
            _ctx: &PackContext<'_>,
            _events: tokio::sync::mpsc::Sender<SessionEvent>,
            _inputs: InputChannel,
            mut cancel: tokio::sync::watch::Receiver<bool>,
        ) -> Result<GoalResult, PackError> {
            loop {
                tokio::select! {
                    _ = cancel.changed() => {
                        self.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                        return Err(PackError::Cancelled);
                    }
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
        }
    }

    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pack = Arc::new(ConcurrentSpyPack {
        cancelled: cancelled.clone(),
    });

    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(pack);
    let hub = Arc::new(hub);

    let session_id = hub
        .start_with_grant(
            GoalSpec {
                id: None,
                description: "concurrent test".into(),
                success_criteria: vec![],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({}),
            },
            SessionGrant {
                capabilities: liberado_common::CapabilitySet::from_iter([
                    liberado_common::Capability::AskHuman,
                ]),
                ..Default::default()
            },
        )
        .await
        .expect("start session");

    // Wait for Running via snapshot loop
    for _ in 0..100 {
        if let Some(snap) = hub.snapshot(&session_id).await
            && snap.session.status == SessionStatus::Running
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Spawn concurrent snapshot + cancel tasks
    let hub_cancel = hub.clone();
    let hub_poll = hub.clone();
    let sid_kill = session_id.clone();
    let sid_poll = session_id.clone();

    let cancel_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = hub_cancel.cancel(&sid_kill).await;
    });
    let poll_task = tokio::spawn(async move {
        for _ in 0..100 {
            if let Some(snap) = hub_poll.snapshot(&sid_poll).await
                && snap.session.status.is_terminal()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        let _ = tokio::join!(cancel_task, poll_task);
    })
    .await
    .expect("concurrent snapshot + cancel must not deadlock");

    // Ground truth: session reached terminal
    let snap = hub.snapshot(&session_id).await.expect("snapshot");
    assert!(
        snap.session.status.is_terminal(),
        "session must be terminal after concurrent park/cancel, got {:?}",
        snap.session.status
    );
    assert_eq!(
        snap.session.status,
        SessionStatus::Cancelled,
        "session must be Cancelled after concurrent cancel"
    );
    assert!(
        cancelled.load(std::sync::atomic::Ordering::SeqCst),
        "pack must have seen cancellation signal"
    );
    // State-machine invariants must hold after terminal.
    liberado_session::check_session_invariants(&snap.session)
        .expect("session invariants violated after concurrent cancel");
}

/// The approval field of the approval mechanism used to be writable by the thing being gated:
/// `proposals/` is `agent_writable` in policy, and `status` sits outside the integrity signature so
/// a human can flip it without invalidating the hash. Each is right alone. Together they meant a
/// note claiming `status: approved` *was* the authorisation.
///
/// It no longer is. Authority is a ledger under `<LIBERADO_DATA_DIR>/`, which no MCP mounts and no
/// tool addresses; the note is a view.
#[tokio::test]
async fn an_approved_note_without_a_recorded_decision_does_not_execute() {
    use liberado_common::{
        ApprovalDecision, ApprovalLedger, Proposal, ProposalSigner, ProposalStatus, ProposedAction,
        ToolCall,
    };

    let signer = ProposalSigner::random();
    let (daemon, dir) = temp_daemon().await;
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = ApprovalLedger::new(ledger_dir.path());
    let daemon = daemon
        .with_proposal_signer(signer.clone())
        .with_approval_ledger(ledger.clone());

    std::fs::create_dir_all(dir.path().join("proposals")).unwrap();

    let proposal = Proposal::pending(
        "prop-forged",
        "corr-forged",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "not authorised" }),
        }]),
        "an agent flipped this",
    );
    // Exactly what an agent's `edit_note` produces: a correctly-signed proposal whose `status` says
    // approved. The signature still verifies — `status` was never covered by it, deliberately.
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);
    assert!(
        daemon.signer.verify(&proposal),
        "the forged note must pass integrity — that is precisely why it needed a second check"
    );

    let rel = Path::new("proposals/prop-forged.md");
    daemon
        .vault
        .write(
            rel,
            &proposal.to_note(),
            None,
            &WriteProvenance::agent("test", "c1"),
        )
        .await
        .unwrap();

    assert!(
        matches!(
            daemon.handle_proposal_change(rel).await.unwrap(),
            ReactionOutcome::Observed
        ),
        "an approved-looking note with no human decision behind it must not execute"
    );

    // ...and a real decision *is* recorded and readable, so this is a gate rather than a blanket
    // refusal that would break approvals entirely.
    ledger
        .record("prop-forged", ApprovalDecision::Approved, "telegram")
        .await
        .unwrap();
    assert_eq!(
        ledger.decision_for("prop-forged").await,
        Some(ApprovalDecision::Approved),
        "the recorded decision is what the daemon consults"
    );
}

/// A daemon built without a ledger approves nothing: a missing security dependency must refuse
/// rather than wave things through.
#[tokio::test]
async fn a_daemon_with_no_ledger_executes_nothing() {
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};

    let signer = ProposalSigner::random();
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon.with_proposal_signer(signer.clone());
    std::fs::create_dir_all(dir.path().join("proposals")).unwrap();

    let proposal = Proposal::pending(
        "prop-noledger",
        "corr-noledger",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "x" }),
        }]),
        "no ledger attached",
    );
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);

    let rel = Path::new("proposals/prop-noledger.md");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &WriteProvenance::human())
        .await
        .unwrap();

    assert!(
        matches!(
            daemon.handle_proposal_change(rel).await.unwrap(),
            ReactionOutcome::Observed
        ),
        "no ledger means no authority to execute under"
    );
}

/// A **permission request** is a proposal, and `handle_proposal_change` is what runs the blocked
/// call, so it passes the same ledger gate as any other. This nearly shipped broken: the ledger was
/// wired into the bot's ordinary approve path and not its permission path, which would have flipped
/// the note, then had the daemon refuse it — a tap that appeared to do nothing.
#[tokio::test]
async fn a_permission_request_also_needs_a_recorded_decision() {
    use liberado_common::{
        Capability, Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall, Zone,
    };

    let signer = ProposalSigner::random();
    let (daemon, dir) = temp_daemon().await;
    let daemon = daemon.with_proposal_signer(signer.clone());
    std::fs::create_dir_all(dir.path().join("proposals")).unwrap();

    let proposal = Proposal::pending(
        "perm-gated",
        "corr-perm",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "needs permission" }),
        }]),
        "a permission request",
    )
    .with_requested_grant(Capability::Write(Zone::vault("sandbox")));
    let mut proposal = signer.sign(proposal);
    proposal.set_status(ProposalStatus::Approved);

    let rel = Path::new("proposals/perm-gated.md");
    daemon
        .vault
        .write(rel, &proposal.to_note(), None, &WriteProvenance::human())
        .await
        .unwrap();

    assert!(
        matches!(
            daemon.handle_proposal_change(rel).await.unwrap(),
            ReactionOutcome::Observed
        ),
        "an approved permission note with no recorded decision must not run the blocked call"
    );

    // With the decision recorded — what a Telegram tap now writes — it is no longer refused here.
    approve_in(&dir, "perm-gated").await;
    assert_eq!(
        test_ledger(&dir).decision_for("perm-gated").await,
        Some(liberado_common::ApprovalDecision::Approved),
    );
}

/// `notify_executed` must actually deliver a notification to the human after a proposal runs.
/// A mutant that drops the body (`()`) is silent — the human never learns the action happened.
/// The call side-effect is the only observable behavior, so spy on the notifier.
#[tokio::test]
async fn notify_executed_sends_a_notification() {
    use liberado_common::{Outcome, Proposal, ProposalSigner, ProposedAction, Report, ToolCall};
    use liberado_notify::Notifier;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct SpyNotifier {
        calls: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl Notifier for SpyNotifier {
        async fn notify(&self, message: &str) -> Result<(), liberado_notify::NotifyError> {
            self.calls.lock().unwrap().push(message.to_string());
            Ok(())
        }
    }

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let notifier = Arc::new(SpyNotifier {
        calls: calls.clone(),
    });
    let (daemon, _dir) = temp_daemon().await;
    let daemon = daemon.with_notifier(notifier);

    let proposal = Proposal::pending(
        "notify:1",
        "notify:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({ "summary": "did a thing" }),
        }]),
        "the human's proposal ran",
    );
    let mut signed = ProposalSigner::random().sign(proposal);
    signed.set_status(liberado_common::ProposalStatus::Approved);
    let proposal = signed.into_proposal();

    let report = Report {
        outcome: Outcome::Succeeded,
        summary: "ran successfully".into(),
        artifacts: Vec::new(),
        new_high_signal_facts: Vec::new(),
        deferred_to_human: false,
        follow_up: None,
        repeat_calls: 0,
    };

    daemon.notify_executed(&proposal, &report).await;

    let logged = calls.lock().unwrap();
    assert!(
        !logged.is_empty(),
        "notify_executed must send a notification to the human"
    );
    assert!(
        logged[0].contains("proposal executed"),
        "notification must report that the proposal executed"
    );
}

/// `proposal_reap_loop` must actually sweep and archive expired approved proposals on its tick.
/// A mutant that drops the body (`()`) returns before ever calling `reap_expired_proposals`,
/// so the expired note sits forever.
#[tokio::test]
async fn proposal_reap_loop_archives_expired_approved_proposal() {
    use chrono::{Duration as ChronoDuration, Utc};
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
    use std::path::Path;
    use std::time::Duration as StdDuration;

    let (daemon, _dir) = temp_daemon().await;
    let vault = daemon.vault.clone();
    let root = vault.root().to_path_buf();
    std::fs::create_dir_all(root.join("proposals")).unwrap();

    let proposal = Proposal::pending(
        "reap-loop:1",
        "reap-loop:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({}),
        }]),
        "expired approved proposal",
    );
    let mut signed = ProposalSigner::random().sign(proposal);
    signed.set_status(ProposalStatus::Approved);
    let mut proposal = signed.into_proposal();
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(2));
    std::fs::write(root.join("proposals/reap-loop-1.md"), proposal.to_note()).unwrap();

    let loop_vault = vault.clone();
    let handle = tokio::spawn(async move {
        crate::proposals::proposal_reap_loop(loop_vault, StdDuration::from_millis(50)).await;
    });
    tokio::time::sleep(StdDuration::from_millis(300)).await;
    handle.abort();

    let archived = vault
        .read(Path::new("proposals/archive/expired/reap-loop-1.md"))
        .await;
    assert!(
        archived.is_ok(),
        "proposal_reap_loop must archive an expired approved proposal"
    );
    assert_eq!(
        Proposal::from_note(&archived.unwrap()).unwrap().status,
        ProposalStatus::Expired
    );
}

/// `reap_expired_proposals` must treat a missing `proposals/` dir as a no-op (Ok), but a
/// directory listing error that is *not* NotFound must propagate as Err. The match-guard
/// mutants (drop the guard, flip `==`, or force it `true`) each change which errors are
/// swallowed; the two cases below pin both arms.
#[tokio::test]
async fn reap_expired_proposals_tolerates_missing_dir_and_non_dir() {
    let (daemon, _dir) = temp_daemon().await;
    let vault = daemon.vault.clone();
    let root = vault.root().to_path_buf();

    // Case 1: ensure no `proposals/` dir -> the NotFound arm must return Ok.
    let proposals_dir = root.join("proposals");
    if proposals_dir.exists() {
        std::fs::remove_dir_all(&proposals_dir).unwrap();
    }
    let result = crate::proposals::reap_expired_proposals(&vault).await;
    assert!(
        result.is_ok(),
        "reaping with no proposals dir must be a no-op Ok, got {:?}",
        result.err()
    );

    // Case 2: `proposals` is a *file* (not a directory) -> the listing error is not NotFound.
    // Original propagates it as Err; a mutant that always takes the Ok arm would swallow it.
    std::fs::write(&proposals_dir, "").unwrap();
    let result = crate::proposals::reap_expired_proposals(&vault).await;
    assert!(
        result.is_err(),
        "a non-NotFound listing error must propagate as Err, not be swallowed"
    );
}

/// `spawn_reaper` must actually launch the background reaper when a non-zero interval is set.
/// A mutant that drops the body (`()`) or flips `!is_zero()` never spawns it, so an expired
/// approved proposal that the reactive path never touches stays in the active dir forever.
#[tokio::test]
async fn spawn_reaper_starts_the_expiry_reaper() {
    use chrono::{Duration as ChronoDuration, Utc};
    use liberado_common::{Proposal, ProposalSigner, ProposalStatus, ProposedAction, ToolCall};
    use std::path::Path;
    use std::time::Duration as StdDuration;

    let (base, _dir) = temp_daemon().await;
    let mut daemon = base.with_proposal_reap_interval(1);
    let vault = daemon.vault.clone();
    let root = vault.root().to_path_buf();
    std::fs::create_dir_all(root.join("proposals")).unwrap();

    let proposal = Proposal::pending(
        "spawn-reap:1",
        "spawn-reap:1",
        "test",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "tasks:create".into(),
            args: serde_json::json!({}),
        }]),
        "expired approved proposal",
    );
    let mut signed = ProposalSigner::random().sign(proposal);
    signed.set_status(ProposalStatus::Approved);
    let mut proposal = signed.into_proposal();
    proposal.expires = Some(Utc::now() - ChronoDuration::hours(2));
    std::fs::write(root.join("proposals/spawn-reap-1.md"), proposal.to_note()).unwrap();

    daemon.spawn_reaper();
    tokio::time::sleep(StdDuration::from_millis(2000)).await;

    let archived = vault
        .read(Path::new("proposals/archive/expired/spawn-reap-1.md"))
        .await;
    assert!(
        archived.is_ok(),
        "spawn_reaper must launch the reaper that archives expired proposals"
    );
    assert_eq!(
        Proposal::from_note(&archived.unwrap()).unwrap().status,
        ProposalStatus::Expired
    );
}

/// `persist_everywhere_grant` must write the machine-owned overlay so an "approve everywhere"
/// grant survives a restart. A mutant that drops the body (`()`) is silently a no-op - the
/// grant the human approved never lands on disk.
#[tokio::test]
async fn persist_everywhere_grant_writes_to_overlay() {
    use liberado_common::{Capability, Zone};
    use std::env;

    let data_dir = tempfile::TempDir::new().unwrap();
    let prev = env::var("LIBERADO_DATA_DIR").ok();
    unsafe {
        env::set_var("LIBERADO_DATA_DIR", data_dir.path());
    }
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                match &self.0 {
                    Some(v) => env::set_var("LIBERADO_DATA_DIR", v),
                    None => env::remove_var("LIBERADO_DATA_DIR"),
                }
            }
        }
    }
    let _restore = Restore(prev);

    let capability = Capability::Write(Zone::vault("tasks"));
    crate::proposals::persist_everywhere_grant("dispatcher", &capability);

    let overlay_path = data_dir.path().join("grants.overlay.toml");
    assert!(
        overlay_path.exists(),
        "persist_everywhere_grant must write the machine-owned overlay"
    );
    let contents = std::fs::read_to_string(&overlay_path).unwrap();
    assert!(
        contents.contains("tasks") && contents.contains("dispatcher"),
        "overlay must record the persisted grant"
    );
}
