use super::*;
use async_trait::async_trait;
use liberado_executor::Budget;
use liberado_provider::{
    CompletionRequest, CompletionResponse, MockProvider, Provider, ProviderResult, Role, ToolDef,
    ToolInvocation,
};
use liberado_session_store::SessionStore;

struct NoTools;
#[async_trait]
impl ToolRuntime for NoTools {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Err("no tools".into())
    }
}

/// A provider whose completion never resolves — lets a turn get started then hang, so a test can
/// cancel it mid-flight by dropping the turn future.
struct PendingProvider;
#[async_trait]
impl Provider for PendingProvider {
    fn model(&self) -> String {
        "pending".into()
    }
    async fn complete(&self, _request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        std::future::pending().await
    }
}

/// A `ChatSessions` over the **real** session store at `root`, scripted with `replies` and no tools.
async fn sessions_at(root: &std::path::Path, replies: Vec<CompletionResponse>) -> ChatSessions {
    let store = Arc::new(SessionStore::open(root).await);
    let provider = Arc::new(MockProvider::with_script("mock", replies));
    let executor = Executor::new(provider, Budget::default());
    ChatSessions::new(store, executor, Arc::new(NoTools))
}

#[tokio::test]
async fn persisted_turn_round_trips_to_disk() {
    let dir = tempfile::tempdir().unwrap();

    let id = {
        let sessions = sessions_at(
            dir.path(),
            vec![CompletionResponse::text("Hi! How can I help?")],
        )
        .await;
        let id = sessions.create(None).await.unwrap();
        let reply = sessions.turn(id, "hello").await.unwrap();
        assert_eq!(reply, "Hi! How can I help?");
        id
    };

    // A SECOND ChatSessions over the SAME store root must see the durable history: it round-trips
    // through disk, not an in-process cache.
    let reopened = sessions_at(dir.path(), Vec::new()).await;
    let history = reopened.history(id).await.unwrap();
    assert_eq!(history[0].role, Role::System);
    assert!(
        history.iter().any(|m| m.content == "hello"),
        "user message did not persist"
    );
    assert!(
        history.iter().any(|m| m.content == "Hi! How can I help?"),
        "assistant reply did not persist"
    );
}

#[tokio::test]
async fn append_note_folds_a_goal_session_summary_into_the_conversation() {
    // The return-handoff path (S4/D2): a finished specialist session's summary is appended to the
    // parent conversation and rehydrates as ordinary context on the next load.
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("On it.")]).await;
        let id = sessions.create(None).await.unwrap();
        sessions.turn(id, "build me a CLI").await.unwrap();
        sessions
            .append_note(
                id,
                "[coding session succeeded] build a hello CLI\nOutcome: 1 file written",
            )
            .await
            .unwrap();
        id
    };

    // Reopen over the same store: the note is durable and in history.
    let reopened = sessions_at(dir.path(), Vec::new()).await;
    let history = reopened.history(id).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| m.content.contains("[coding session succeeded]")
                && m.content.contains("1 file written")),
        "handoff note did not persist into the conversation"
    );
}

#[tokio::test]
async fn context_carries_across_turns_via_rehydration() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("Hi! How can I help?"),
            CompletionResponse::text("You said hello a moment ago."),
        ],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools));

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "hello").await.unwrap();
    sessions.turn(id, "what did I just say?").await.unwrap();

    // The second turn rehydrated from the store, so its provider request carried the first user
    // message — context survived even though nothing was held in memory between turns.
    let second_request = &provider.received_requests()[1];
    assert!(
        second_request.messages.iter().any(|m| m.content == "hello"),
        "rehydration lost the first user message"
    );
}

#[tokio::test]
async fn cancelled_stream_persists_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools));

    let id = sessions.create(None).await.unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(8);

    // Start the streaming turn, poll once to push the user message and issue the (hanging)
    // request, then drop the future to simulate the client stopping mid-turn.
    {
        let fut = sessions.turn_stream(id, "hi", &tx);
        tokio::pin!(fut);
        assert!(
            futures::poll!(fut.as_mut()).is_pending(),
            "the pending provider should leave the turn in flight"
        );
    } // fut dropped here

    // The store holds only the system prompt — the cancelled turn wrote nothing.
    let history = sessions.history(id).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].role, Role::System);
}

#[tokio::test]
async fn list_returns_created_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), Vec::new()).await;

    sessions.create(Some("My chat".into())).await.unwrap();
    let headers = sessions.list().await.unwrap();
    assert!(
        headers
            .iter()
            .any(|h| h.title.as_deref() == Some("My chat")),
        "list did not return the created conversation"
    );
}

#[test]
fn default_title_uses_first_nonempty_line() {
    assert_eq!(
        default_conversation_title("  hello world  \nsecond line"),
        "hello world"
    );
    assert_eq!(default_conversation_title("\n\n  hi  "), "hi");
    assert_eq!(default_conversation_title("   \n  "), "");
}

#[test]
fn default_title_collapses_whitespace_and_truncates() {
    assert_eq!(
        default_conversation_title("too   many\t spaces"),
        "too many spaces"
    );
    let long = "x".repeat(100);
    let t = default_conversation_title(&long);
    assert_eq!(t.chars().count(), 72);
    assert!(t.ends_with('…'));
}

#[tokio::test]
async fn turn_seeds_title_from_first_user_line() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("ok")]).await;
    let id = sessions.create(None).await.unwrap();
    sessions
        .turn(id, "Plan a trip to Lisbon\nwith details")
        .await
        .unwrap();
    let headers = sessions.list().await.unwrap();
    let h = headers.iter().find(|h| h.id == id).unwrap();
    assert_eq!(h.title.as_deref(), Some("Plan a trip to Lisbon"));
}

#[tokio::test]
async fn seed_does_not_overwrite_explicit_title() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("ok")]).await;
    let id = sessions.create(Some("Pinned name".into())).await.unwrap();
    sessions
        .turn(id, "this should not become the title")
        .await
        .unwrap();
    let header = sessions.list().await.unwrap();
    let h = header.iter().find(|h| h.id == id).unwrap();
    assert_eq!(h.title.as_deref(), Some("Pinned name"));
}

#[tokio::test]
async fn list_backfills_title_from_existing_user_message() {
    use liberado_conversation_store::{Author, ConversationStore, NewConversation, NewNode};
    use liberado_provider::Message;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    // Pre-seed era: header with no title + a user message already on disk.
    let header = store
        .create(NewConversation {
            title: None,
            parent_conversation: None,
            spawned_by: None,
        })
        .await
        .unwrap();
    store
        .append(
            header.id,
            NewNode {
                parent_id: None,
                author: Author::User,
                message: Message::user("Buy milk and eggs"),
            },
        )
        .await
        .unwrap();

    let sessions = sessions_at(dir.path(), Vec::new()).await;
    let headers = sessions.list().await.unwrap();
    let h = headers.iter().find(|h| h.id == header.id).unwrap();
    assert_eq!(h.title.as_deref(), Some("Buy milk and eggs"));

    // Second list is a no-op overwrite of the same default (title already Some).
    let headers2 = sessions.list().await.unwrap();
    assert_eq!(
        headers2
            .iter()
            .find(|h| h.id == header.id)
            .unwrap()
            .title
            .as_deref(),
        Some("Buy milk and eggs")
    );
}

#[tokio::test]
async fn guarded_turn_with_risk_gated_runtime_works() {
    // Verify that a ChatSessions with guards configured can still run a turn successfully.
    // The inner runtime has no tools, so the advisor should find nothing, and the turn
    // should complete as a pure conversation.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("Hello!")],
    ));
    let executor = Executor::new(provider, Budget::default());

    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_guards(
        vec![("tasks-mcp".into(), Consequence::Reversible)],
        liberado_common::CapabilitySet::empty(),
        dir.path().join("proposals"),
        ProposalSigner::random(),
    );

    let id = sessions.create(None).await.unwrap();
    let reply = sessions.turn(id, "hello").await.unwrap();
    assert_eq!(reply, "Hello!");
}

/// A runtime that always offers one tool, so we can assert what the model is shown.
struct OneTool(&'static str);
#[async_trait]
impl ToolRuntime for OneTool {
    fn catalog(&self) -> Vec<ToolDef> {
        vec![ToolDef::new(
            self.0,
            "a tool",
            serde_json::json!({ "type": "object" }),
        )]
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Ok("ok".into())
    }
}

#[tokio::test]
async fn granted_mcp_tools_surface_regardless_of_phrasing() {
    // A granted MCP's tools must be offered to the model even when the message is phrased
    // without an action verb (the case the removed verb-list advisor used to drop).
    use liberado_common::{Capability, CapabilitySet};

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("It's empty.")],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());

    let sessions = ChatSessions::new(store, executor, Arc::new(OneTool("calendar-mcp:list")))
        .with_guards(
            vec![("calendar-mcp".into(), Consequence::Reversible)],
            CapabilitySet::from_iter([Capability::ExecuteMcp("calendar-mcp".into())]),
            dir.path().join("proposals"),
            ProposalSigner::random(),
        );

    let id = sessions.create(None).await.unwrap();
    // No action verb — the old advisor would have surfaced zero tools here.
    sessions.turn(id, "what's on my calendar?").await.unwrap();

    let offered: Vec<String> = provider.received_requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(
        offered.contains(&"calendar-mcp:list".to_string()),
        "granted MCP tool must be surfaced regardless of phrasing; got {offered:?}"
    );
}

#[tokio::test]
async fn ungranted_mcp_tools_are_scoped_out() {
    // A configured-but-ungranted MCP must not be surfaced (capability scoping holds).
    use liberado_common::CapabilitySet;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("ok")],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());

    let sessions = ChatSessions::new(store, executor, Arc::new(OneTool("email-mcp:send")))
        .with_guards(
            vec![("email-mcp".into(), Consequence::External)],
            CapabilitySet::empty(), // nothing granted
            dir.path().join("proposals"),
            ProposalSigner::random(),
        );

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "list my email").await.unwrap();

    let offered: Vec<String> = provider.received_requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(
        offered.is_empty(),
        "ungranted MCP tools must be scoped out; got {offered:?}"
    );
}

// ── Dispatch routing ─────────────────────────────────────────────────────

use liberado_common::{BlockReason, DispatchDecision};
use liberado_config_loader::DispatchTuning;
use liberado_executor::{RuntimeFactory, RuntimeSetupError};

struct NoopFactory;
#[async_trait]
impl RuntimeFactory for NoopFactory {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: liberado_common::WriteProvenance,
    ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
        unreachable!("Clarify never builds a runtime")
    }
}

/// A `ChatSessions` with dispatch classification + a hub that hosts the dispatch pack.
/// `dispatch_decision` scripts the classifier; `chat_replies` scripts the plain conversational
/// executor for the `ExecuteDirect` fallthrough case; `worker_script` scripts the pack's worker
/// for non-`ExecuteDirect` outcomes.
async fn sessions_with_dispatch(
    root: &std::path::Path,
    dispatch_decision: DispatchDecision,
    chat_replies: Vec<CompletionResponse>,
    worker_script: Vec<CompletionResponse>,
) -> ChatSessions {
    use liberado_dispatch_pack::DispatchPack;
    use liberado_orchestrator::Orchestrator;
    use liberado_session::{GoalSessionHub, GoalSessionStore};

    let store = Arc::new(SessionStore::open(root).await);
    let dispatch_provider = Arc::new(MockProvider::with_script(
        "dispatch",
        [CompletionResponse::text(
            serde_json::to_string(&dispatch_decision).unwrap(),
        )],
    ));
    let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

    // A second dispatcher for the pack (the pack owns classify+execute).
    let pack_dispatcher = Dispatcher::new(
        Arc::new(MockProvider::with_script(
            "pack-dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&dispatch_decision).unwrap(),
            )],
        )),
        DispatchTuning::default(),
        4,
    );
    let pack_orchestrator = Orchestrator::new(
        Arc::new(MockProvider::with_script("pack-exec", worker_script)),
        NoopFactory,
        CapabilitySet::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        std::env::temp_dir(),
        ProposalSigner::random(),
        "default",
    );
    let proposals_dir = root.join("data");
    let pack = DispatchPack::new(
        Arc::new(CapabilityCatalog::new()),
        Vec::new(),
        1,
        proposals_dir.clone(),
    )
    .with_pool("default", pack_dispatcher, pack_orchestrator);
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(pack));
    let hub = Arc::new(hub);

    let chat_provider = Arc::new(MockProvider::with_script("chat", chat_replies));
    let executor = Executor::new(chat_provider, liberado_executor::Budget::default());

    ChatSessions::new(store, executor, Arc::new(NoTools))
        .with_goal_hub(hub)
        .with_guards(
            Vec::new(),
            CapabilitySet::empty(),
            proposals_dir,
            ProposalSigner::random(),
        )
        .with_dispatch(dispatcher, Arc::new(CapabilityCatalog::new()))
}

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

/// A runtime offering tools from two different MCP namespaces, so narrowing between them is
/// observable.
struct TwoMcpTools;
#[async_trait]
impl ToolRuntime for TwoMcpTools {
    fn catalog(&self) -> Vec<ToolDef> {
        vec![
            ToolDef::new(
                "tasks-mcp:add",
                "add a task",
                serde_json::json!({ "type": "object" }),
            ),
            ToolDef::new(
                "email-mcp:send",
                "send an email",
                serde_json::json!({ "type": "object" }),
            ),
        ]
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Ok("ok".into())
    }
}

/// Build a `ChatSessions` granted both `tasks-mcp` and `email-mcp`, with dispatch attached and
/// scripted to return `relevant_mcps`, for testing dispatcher-narrowed tool surfacing.
async fn sessions_for_narrowing_test(
    dir: &std::path::Path,
    relevant_mcps: Vec<String>,
) -> (ChatSessions, Arc<MockProvider>) {
    use liberado_common::Capability;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps,
        },
        confidence: 0.9,
        rationale: "test".into(),
    };
    let dispatch_provider = Arc::new(MockProvider::with_script(
        "dispatch",
        [CompletionResponse::text(
            serde_json::to_string(&decision).unwrap(),
        )],
    ));
    let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

    let chat_provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text("done")],
    ));
    let executor = Executor::new(chat_provider.clone(), Budget::default());
    let store = Arc::new(SessionStore::open(dir).await);
    let capabilities = CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::ExecuteMcp("email-mcp".into()),
    ]);
    let sessions = ChatSessions::new(store, executor, Arc::new(TwoMcpTools))
        .with_guards(
            Vec::new(),
            capabilities,
            dir.join("proposals"),
            ProposalSigner::random(),
        )
        .with_dispatch(dispatcher, Arc::new(CapabilityCatalog::new()));

    (sessions, chat_provider)
}

#[tokio::test]
async fn execute_direct_relevant_mcps_narrows_the_surfaced_tools() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, chat_provider) =
        sessions_for_narrowing_test(dir.path(), vec!["tasks-mcp".into()]).await;

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "add milk to my list").await.unwrap();

    let offered: Vec<String> = chat_provider.received_requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(
        offered,
        vec!["tasks-mcp:add".to_string()],
        "narrowed to only the relevant MCP, not the full grant"
    );
}

#[tokio::test]
async fn execute_direct_empty_relevant_mcps_falls_back_to_full_grant() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, chat_provider) = sessions_for_narrowing_test(dir.path(), Vec::new()).await;

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "do something").await.unwrap();

    let mut offered: Vec<String> = chat_provider.received_requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    offered.sort();
    assert_eq!(
        offered,
        vec!["email-mcp:send".to_string(), "tasks-mcp:add".to_string()],
        "empty relevant_mcps must fall back to the full grant"
    );
}
