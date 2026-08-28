//! Face / clarify / propose / execute decisions and background delegation.

use super::super::*;
use super::test_fixtures::*;

#[tokio::test]
async fn a_delegated_subagent_becomes_a_background_session_under_the_chat_that_asked_for_it() {
    // E4: `delegate` starts a hosted hub session (dispatch pack). Visible while it runs, terminal
    // when done, child of the conversation that asked for it.
    use liberado_dispatch_pack::DispatchPack;
    use liberado_orchestrator::Orchestrator;
    use liberado_session::{GoalSessionHub, GoalSessionStore, SessionStatus, Visibility};

    let dir = tempfile::tempdir().unwrap();

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "routine lookup".into(),
    };
    let pack_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        )),
        DispatchTuning::default(),
        4,
    );
    let pack_orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                liberado_executor::SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "found 3 open tasks" }),
            )])],
        )),
        NoopFactory,
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
    .with_pool("default", pack_dispatcher, pack_orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    // The face agent calls `delegate`, then summarizes for the human.
    let chat_provider = Arc::new(MockProvider::with_script(
        "chat",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "d1",
                crate::DELEGATE_TOOL_NAME,
                serde_json::json!({ "goal": "how many open tasks do I have?" }),
            )]),
            CompletionResponse::text("You have 3 open tasks."),
        ],
    ));

    let chat = ChatSessions::new(
        Arc::new(SessionStore::open(dir.path()).await),
        Executor::new(chat_provider, liberado_executor::Budget::default()),
        Arc::new(NoTools),
    )
    .with_delegation_mode(true)
    .with_goal_hub(hub.clone())
    .with_dispatch(
        Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "unused",
                Vec::<CompletionResponse>::new(),
            )),
            DispatchTuning::default(),
            4,
        ),
        Arc::new(CapabilityCatalog::new()),
    );

    let chat_id = chat.create(None).await.unwrap();
    chat.turn(chat_id, "how many open tasks do I have?")
        .await
        .unwrap();

    let rows = hub.list().await;
    assert_eq!(rows.len(), 1, "the delegation must be exactly one session");
    let row = &rows[0];

    assert_eq!(row.visibility, Visibility::Background);
    assert_eq!(row.goal.description, "how many open tasks do I have?");
    assert_eq!(row.status, SessionStatus::Succeeded);
    assert_eq!(row.result.as_ref().unwrap().summary, "found 3 open tasks");

    // The edge that makes it *findable*: this session hangs off the chat that delegated it, so
    // "what did that actually do?" is a question with an answer you can open.
    let origin = row.goal.origin.as_ref().expect("a subagent has an origin");
    assert_eq!(
        origin.conversation_id.as_deref(),
        Some(chat_id.to_string().as_str()),
        "the delegation must be a child of the chat that asked for it"
    );
    assert!(
        origin
            .correlation_id
            .as_deref()
            .unwrap()
            .starts_with("chat-delegate-"),
        "and still stitched to its dispatch journal entry"
    );
}

#[tokio::test]
async fn face_agent_surfaces_only_delegate_by_default() {
    use liberado_session::{GoalSessionHub, GoalSessionStore};
    let dir = tempfile::tempdir().unwrap();
    // Face agent: model replies in prose without calling tools (just verify catalog).
    let chat_provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text(
            "Happy to help — what do you need?",
        )],
    ));
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );
    let hub = Arc::new(GoalSessionHub::new(GoalSessionStore::new()));
    let executor = Executor::new(chat_provider.clone(), liberado_executor::Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools))
        .with_delegation_mode(true)
        .with_goal_hub(hub)
        .with_dispatch(dispatcher, Arc::new(CapabilityCatalog::new()));

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "hello").await.unwrap();

    let offered: Vec<String> = chat_provider.received_requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(
        offered,
        vec![crate::DELEGATE_TOOL_NAME.to_string()],
        "face agent should only see delegate; got {offered:?}"
    );
}

#[tokio::test]
async fn clarify_decision_answers_without_executing() {
    let dir = tempfile::tempdir().unwrap();
    let decision = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["which vault folder do you mean?".into()],
            what_blocked: BlockReason::Ambiguous,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    // The chat-path provider script is never touched — the turn is answered via a hub session
    // (dispatch pack) before any conversational execution happens.
    let sessions = sessions_with_dispatch(dir.path(), decision, Vec::new(), vec![]).await;

    let id = sessions.create(None).await.unwrap();
    let reply = sessions.turn(id, "clean up my notes").await.unwrap();
    assert!(
        reply.contains("which vault folder do you mean?"),
        "got: {reply}"
    );

    // Persisted like any other turn: user message + assistant reply.
    let history = sessions.history(id).await.unwrap();
    assert!(history.iter().any(|m| m.content == "clean up my notes"));
    assert!(
        history
            .iter()
            .any(|m| m.content.contains("which vault folder do you mean?"))
    );
}

#[tokio::test]
async fn execute_direct_decision_falls_through_to_normal_execution() {
    let dir = tempfile::tempdir().unwrap();
    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: Vec::new(),
            delivery: Delivery::Summarize,
        },
        confidence: 0.95,
        rationale: "trivial".into(),
    };
    // ExecuteDirect falls through to the chat path — the pack worker is never invoked.
    let sessions = sessions_with_dispatch(
        dir.path(),
        decision,
        vec![CompletionResponse::text("Hello from the normal path!")],
        vec![],
    )
    .await;

    let id = sessions.create(None).await.unwrap();
    let reply = sessions.turn(id, "hello").await.unwrap();
    assert_eq!(reply, "Hello from the normal path!");
}

#[tokio::test]
async fn propose_decision_writes_a_proposal_file_and_confirms() {
    use liberado_common::{Proposal, ProposalStatus, ProposedAction};

    let dir = tempfile::tempdir().unwrap();
    let decision = DispatchDecision {
        action: DispatchAction::Propose {
            proposed_action: ProposedAction::External {
                description: "send the weekly report email".into(),
            },
            rationale: "sending email is external".into(),
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    // Propose is handled by the dispatch pack (writes the proposal file itself).
    let sessions = sessions_with_dispatch(dir.path(), decision, Vec::new(), vec![]).await;

    let id = sessions.create(None).await.unwrap();
    let reply = sessions
        .turn(id, "email the team the weekly report")
        .await
        .unwrap();

    assert!(
        reply.contains("proposal"),
        "expected a proposal confirmation, got: {reply}"
    );
    let proposals_dir = dir.path().join("data").join("proposals");
    let entries: Vec<_> = std::fs::read_dir(&proposals_dir)
        .expect("proposals dir should exist")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(entries.len(), 1, "exactly one proposal file written");
    let contents = std::fs::read_to_string(entries[0].path()).unwrap();
    let parsed = Proposal::from_note(&contents).expect("proposal note round-trips");
    assert_eq!(parsed.status, ProposalStatus::Pending);
}

#[test]
fn collapse_if_deferred_replaces_reply_only_when_flagged() {
    // Gap 2: when a delegate deferred out-of-band, the face agent's reply collapses to the tiny
    // waiting marker; otherwise it passes through untouched.
    let flag = AtomicBool::new(false);
    assert_eq!(
        collapse_if_deferred("here are your tasks".into(), &flag),
        "here are your tasks",
        "no deferral → reply is unchanged"
    );

    flag.store(true, Ordering::Relaxed);
    assert_eq!(
        collapse_if_deferred("I've asked for permission to send the email".into(), &flag),
        DEFERRED_REPLY_MARKER,
        "a deferral collapses the redundant reply to the waiting marker"
    );
}

/// Inner runtime that would write if RiskGated let the call through.
struct WouldWrite;
#[async_trait]
impl ToolRuntime for WouldWrite {
    fn catalog(&self) -> Vec<ToolDef> {
        vec![ToolDef::new(
            "vault:write_note",
            "write a note",
            serde_json::json!({"type": "object"}),
        )]
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Ok("WRITTEN".into())
    }
}

#[tokio::test]
async fn face_extras_with_empty_boot_consequences_still_gate_via_live_catalog_after_peer_apply() {
    // Empty boot-time consequence snapshot + live catalog + ExecuteMcp grant: face extras must
    // still refuse a zoned write without Write(zone) after the peer is registered (hot-reload).
    use liberado_common::{
        Capability, CapabilitySet, Consequence, McpDescriptor, ProposalSigner, WriteClass,
    };
    use liberado_session::{GoalSessionHub, GoalSessionStore};

    let dir = tempfile::tempdir().unwrap();
    let catalog = Arc::new(CapabilityCatalog::new());
    // Boot: empty catalog (no consequence snapshot material).
    assert!(catalog.is_empty());

    // Hot-reload: path-addressed write peer appears on the live catalog.
    catalog.register(McpDescriptor {
        name: "vault".into(),
        description: "path-addressed vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec!["write_note".into()],
    });

    let provider = Arc::new(MockProvider::with_script(
        "chat",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c1",
                "vault:write_note",
                serde_json::json!({"path": "tasks/x.md"}),
            )]),
            // If the gate lets the write through, the model would see WRITTEN and may continue;
            // we assert the transcript never contains a successful write result.
            CompletionResponse::text("done"),
        ],
    ));
    let hub = Arc::new(GoalSessionHub::new(GoalSessionStore::new()));
    let dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "unused",
            Vec::<CompletionResponse>::new(),
        )),
        DispatchTuning::default(),
        4,
    );

    let sessions = ChatSessions::new(
        Arc::new(SessionStore::open(dir.path()).await),
        Executor::new(provider, Budget::default()),
        Arc::new(WouldWrite),
    )
    .with_delegation_mode(true)
    .with_goal_hub(hub)
    .with_dispatch(dispatcher, catalog.clone())
    // Boot-empty consequences (the bug path) — live catalog must still gate.
    .with_guards(
        Vec::new(),
        CapabilitySet::from_iter([Capability::ExecuteMcp("vault".into())]),
        dir.path().join("proposals"),
        ProposalSigner::random(),
    )
    .with_zone_guards(
        Vec::new(), // empty zone snapshot at boot
        vec![("tasks".into(), WriteClass::AgentWritable)],
    )
    .with_live_catalog(catalog);

    let id = sessions.create(None).await.unwrap();
    let reply = sessions.turn(id, "write a note").await.unwrap();
    assert_ne!(
        reply, "WRITTEN",
        "face extras must not execute the write when only ExecuteMcp is granted"
    );
    // History must not contain a tool result of a successful write.
    let history = sessions.history(id).await.unwrap();
    let texts: Vec<&str> = history.iter().map(|m| m.content.as_str()).collect();
    assert!(
        texts.iter().all(|t| !t.contains("WRITTEN")),
        "ungated write must not land in transcript: {texts:?}"
    );
}

/// `with_delegation_mode(false)` must leave the default prompt alone; the swapped prompt is
/// only for face mode.
#[test]
fn disabling_delegation_keeps_the_default_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let store = Arc::new(SessionStore::open(dir.path()).await);
        let executor = Executor::new(
            Arc::new(MockProvider::with_script(
                "m",
                Vec::<CompletionResponse>::new(),
            )),
            Budget::default(),
        );
        let plain =
            ChatSessions::new(store, executor, Arc::new(NoTools)).with_delegation_mode(false);
        assert_eq!(
            plain.system_prompt,
            crate::DEFAULT_SYSTEM_PROMPT,
            "delegation off must not install the face prompt"
        );

        let store2 = Arc::new(SessionStore::open(dir.path().join("b")).await);
        let executor2 = Executor::new(
            Arc::new(MockProvider::with_script(
                "m2",
                Vec::<CompletionResponse>::new(),
            )),
            Budget::default(),
        );
        let face =
            ChatSessions::new(store2, executor2, Arc::new(NoTools)).with_delegation_mode(true);
        assert_eq!(face.system_prompt, crate::HUMAN_INTERFACE_SYSTEM_PROMPT);
    });
}

/// Face-agent resolution is a conjunction: either half missing means direct execution.
#[tokio::test]
async fn uses_face_agent_requires_both_delegation_and_a_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(
        Arc::new(MockProvider::with_script(
            "m",
            Vec::<CompletionResponse>::new(),
        )),
        Budget::default(),
    );
    let bare = ChatSessions::new(store, executor, Arc::new(NoTools));
    assert!(
        !bare.uses_face_agent(true),
        "delegation with no hub is still the direct path"
    );
    assert!(!bare.uses_face_agent(false));

    let store2 = Arc::new(SessionStore::open(dir.path().join("b")).await);
    let executor2 = Executor::new(
        Arc::new(MockProvider::with_script(
            "m2",
            Vec::<CompletionResponse>::new(),
        )),
        Budget::default(),
    );
    let bridged = ChatSessions::new(store2, executor2, Arc::new(NoTools))
        .with_goal_hub(Arc::new(liberado_session::GoalSessionHub::new(
            liberado_session::GoalSessionStore::new(),
        )))
        .with_delegation_mode(true);
    assert!(bridged.uses_face_agent(true));
    assert!(!bridged.uses_face_agent(false));
}

/// The streamed face turn keeps the human-interface prompt — an inverted `!face_agent` guard
/// would swap it for the direct-agent prompt exactly when the session IS a face agent.
#[tokio::test]
async fn streamed_face_turn_keeps_the_face_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let chat_provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text("hello from your face agent")],
    ));
    let executor = Executor::new(chat_provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools))
        .with_goal_hub(Arc::new(liberado_session::GoalSessionHub::new(
            liberado_session::GoalSessionStore::new(),
        )))
        .with_delegation_mode(true);
    let id = sessions.create(None).await.unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    sessions.turn_stream(id, "hi", &tx).await.unwrap();
    drop(tx);

    let sent = &chat_provider.received_requests()[0];
    let system = sent
        .messages
        .iter()
        .find(|m| m.role == Role::System)
        .expect("a system prompt is sent");
    assert_eq!(
        system.content,
        crate::HUMAN_INTERFACE_SYSTEM_PROMPT,
        "a face session must run the face prompt on the streaming path too"
    );
    while rx.try_recv().is_ok() {}
}

/// The pre-turn dispatch hands the hosted session its capability ceiling with AskHuman
/// stripped (D-e) — not an empty grant, and not AskHuman-only.
#[tokio::test]
async fn pre_turn_dispatch_grant_strips_askhuman_keeps_the_rest() {
    use liberado_orchestrator::Orchestrator;

    let dir = tempfile::tempdir().unwrap();
    let decision = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["which list?".into()],
            what_blocked: BlockReason::Ambiguous,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    // One scripted classifier serves both the chat-side dispatcher and the pack's own.
    let decision_json = serde_json::to_string(&decision).unwrap();
    let dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(decision_json.clone())],
        )),
        DispatchTuning::default(),
        4,
    );
    let pack_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "pack-dispatch",
            [CompletionResponse::text(decision_json)],
        )),
        DispatchTuning::default(),
        4,
    );
    let pack_orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script(
            "pack-exec",
            [CompletionResponse::text("done")],
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
    let pack = liberado_dispatch_pack::DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        dir.path().join("proposals"),
    )
    .with_pool("default", pack_dispatcher, pack_orchestrator);
    let mut goal_hub =
        liberado_session::GoalSessionHub::new(liberado_session::GoalSessionStore::new());
    goal_hub.register_pack(Arc::new(pack));
    let hub = Arc::new(goal_hub);

    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(
        Arc::new(MockProvider::with_script(
            "chat",
            [CompletionResponse::text("relay")],
        )),
        Budget::default(),
    );
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools))
        .with_goal_hub(hub.clone())
        .with_dispatcher_capabilities(CapabilitySet::from_iter([
            Capability::ExecuteMcp("tasks-mcp".into()),
            Capability::AskHuman,
        ]))
        .with_dispatch(dispatcher, Arc::new(CapabilityCatalog::new()));

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "add something").await.unwrap();

    let rows = hub.list().await;
    assert_eq!(rows.len(), 1, "the pre-turn dispatch hosted one session");
    let grant = &rows[0].grant;
    assert!(
        grant
            .capabilities
            .capabilities
            .iter()
            .any(|c| matches!(c, Capability::ExecuteMcp(m) if m == "tasks-mcp")),
        "the real ceiling reaches the hosted session: {:?}",
        grant.capabilities
    );
    assert!(
        !grant.grants_ask_human(),
        "AskHuman must be stripped from pre-turn dispatch too"
    );
}
