use super::*;
use async_trait::async_trait;
use liberado_conversation_store::{ConversationStore, StoreError};
use liberado_executor::Budget;
use liberado_provider::{
    CompletionRequest, CompletionResponse, MockProvider, Provider, ProviderError, ProviderResult,
    Role, ToolDef, ToolInvocation,
};
use liberado_session_store::SessionStore;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

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

#[tokio::test]
async fn live_catalog_without_grants_does_not_pass_through_all_registry_tools() {
    // empty capabilities + empty consequences + live_catalog must not PassThrough the full
    // LiveRegistryRuntime (regression vs boot-None MCP → NoTools).
    use liberado_common::{CapabilitySet, ProposalSigner};

    let dir = tempfile::tempdir().unwrap();
    let catalog = Arc::new(CapabilityCatalog::new());
    catalog.register(liberado_common::McpDescriptor {
        name: "tasks".into(),
        description: "tasks".into(),
        consequence: liberado_common::Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
    });

    let provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text("ok")],
    ));
    let sessions = ChatSessions::new(
        Arc::new(SessionStore::open(dir.path()).await),
        Executor::new(provider.clone(), Budget::default()),
        Arc::new(OneTool("tasks:add")),
    )
    .with_guards(
        Vec::new(),
        CapabilitySet::empty(),
        dir.path().join("proposals"),
        ProposalSigner::random(),
    )
    .with_live_catalog(catalog);

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "add a task").await.unwrap();

    let offered: Vec<String> = provider.received_requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(
        offered.is_empty() || !offered.iter().any(|n| n == "tasks:add"),
        "empty grants + live catalog must not surface peer tools unscoped; got {offered:?}"
    );
}

// ── CH3: context compaction ─────────────────────────────────────────────────
//
// Every test here runs against the **real** `SessionStore` (the store production constructs) and
// asserts on what the *provider actually received* — the two doctrine points of
// `docs/architecture/failure-modes.md` §1. The load-bearing assertion of the first test (raw
// elided content ABSENT from the post-compaction request) fails if compaction is neutered to a
// no-op — break the code, watch the test fail.

/// A `ChatSessions` with compaction wired, over the real session store. One `MockProvider` serves
/// **both** the executor and the summarizer, so the script interleaves in call order (a
/// compaction's summary request consumes the next scripted response before the turn's reply).
async fn compacting_sessions_at(
    root: &std::path::Path,
    config: CompactionConfig,
    replies: Vec<CompletionResponse>,
) -> (ChatSessions, Arc<MockProvider>) {
    let store = Arc::new(SessionStore::open(root).await);
    let provider = Arc::new(MockProvider::with_script("mock", replies));
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools))
        .with_compaction(config, provider.clone());
    (sessions, provider)
}

/// Append user/assistant pairs directly to the store (bypassing turns, so seeding never consumes
/// scripted replies nor trips the compaction trigger mid-seed).
async fn seed_turns(sessions: &ChatSessions, id: Ulid, pairs: &[(&str, &str)]) {
    let mut parent = sessions
        .store
        .leaf_path(id, None)
        .await
        .unwrap()
        .last()
        .map(|n| n.id);
    for (u, a) in pairs {
        for (author, msg) in [
            (Author::User, Message::user(*u)),
            (Author::Assistant, Message::assistant(*a)),
        ] {
            let node = sessions
                .store
                .append(
                    id,
                    NewNode {
                        parent_id: parent,
                        author,
                        message: msg,
                    },
                )
                .await
                .unwrap();
            parent = Some(node.id);
        }
    }
}

#[tokio::test]
async fn compacts_over_trigger_and_the_next_turn_sees_the_summary_not_the_raw_history() {
    let dir = tempfile::tempdir().unwrap();
    let summary = "SUMMARY: earlier chit-chat about squirrels".to_string();

    // Sized so the seeded history (four ~600-char messages) far exceeds it, while the
    // post-compaction view (system + marker + short tail + short first turn + the next incoming
    // question) lands exactly AT it — so turn 1 compacts and turn 2 provably does not.
    let trigger = compaction::estimate_tokens(&[
        Message::system(DEFAULT_SYSTEM_PROMPT),
        compaction::marker_message(&summary),
        Message::user("tail question"),
        Message::assistant("tail answer"),
        Message::user("fresh question"),
        Message::assistant("fresh answer"),
        Message::user("second question"),
    ]);
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: trigger,
        keep_recent_turns: 1,
        summary_max_tokens: 512,
        tool_result_max_chars: 2_000,
    };
    let (sessions, provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text(summary.clone()),
            CompletionResponse::text("fresh answer"),
            CompletionResponse::text("second answer"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    let secret = format!("SECRET-ELIDED-{}", "x".repeat(600));
    seed_turns(
        &sessions,
        id,
        &[
            (&secret, &format!("A1 {}", "y".repeat(600))),
            (
                &format!("u2 {}", "z".repeat(600)),
                &format!("A2 {}", "w".repeat(600)),
            ),
            ("tail question", "tail answer"),
        ],
    )
    .await;

    // Turn 1: over the trigger → summarize, persist the marker, run on the compacted view.
    let reply = sessions.turn(id, "fresh question").await.unwrap();
    assert_eq!(reply, "fresh answer");

    let requests = provider.received_requests();
    assert_eq!(requests.len(), 2, "summarizer + one turn completion");
    // The summarizer's input is where the elided content legitimately goes…
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.content.contains("SECRET-ELIDED")),
        "the elided region must reach the summarizer's transcript"
    );
    // …and the turn request is where it must NOT go. This is the assertion that fails if
    // compaction is broken into a no-op: the raw history would ride every request forever.
    let turn_req = &requests[1];
    assert!(
        turn_req.messages.iter().any(|m| m
            .content
            .contains("SUMMARY: earlier chit-chat about squirrels")),
        "the compacted view must carry the rolling summary"
    );
    assert!(
        !turn_req
            .messages
            .iter()
            .any(|m| m.content.contains("SECRET-ELIDED")),
        "elided history must not reach the model after compaction"
    );
    assert!(
        turn_req
            .messages
            .iter()
            .any(|m| m.content == "tail question")
            && turn_req
                .messages
                .iter()
                .any(|m| m.content == "fresh question"),
        "the kept tail and the incoming message must survive verbatim"
    );

    // The full rendered history keeps EVERYTHING — marker included, raw elided content intact
    // (compaction never deletes; it only changes what the model sees).
    let history = sessions.history(id).await.unwrap();
    assert!(history.iter().any(|m| m.content.contains("SECRET-ELIDED")));
    assert!(
        history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "rendered history must show the compaction marker"
    );

    // Turn 2: under the trigger now, so no second summarization — and the marker persisted, so
    // the next load still resumes from the summary, not the raw history.
    let reply2 = sessions.turn(id, "second question").await.unwrap();
    assert_eq!(reply2, "second answer");
    let requests = provider.received_requests();
    assert_eq!(
        requests.len(),
        3,
        "no second summarization should have run (view is under the trigger)"
    );
    let turn2 = &requests[2];
    assert!(
        turn2.messages.iter().any(|m| m
            .content
            .contains("SUMMARY: earlier chit-chat about squirrels")),
        "the marker must persist across loads"
    );
    assert!(
        !turn2
            .messages
            .iter()
            .any(|m| m.content.contains("SECRET-ELIDED")),
        "the elision rule must hold on the next load too"
    );
}

/// Rolling compaction: a second fire must fold the previous marker into the summarizer transcript
/// and replace it with a new summary — not re-summarize only the post-marker slice and drop prior
/// facts. Break-check for the "rolling update" claim that was previously only live-verified.
#[tokio::test]
async fn second_compaction_rolls_prior_summary_forward() {
    let dir = tempfile::tempdir().unwrap();
    let summary_a = "SUMMARY-A: code word is ALPHA".to_string();
    let summary_b = "SUMMARY-B: code word is ALPHA; later topic is BETA".to_string();

    // Always-fire trigger so both turns compact; keep_recent_turns=1 so each fire has something to
    // elide once we have grown past a single user turn after the previous marker.
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1,
        keep_recent_turns: 1,
        summary_max_tokens: 512,
        tool_result_max_chars: 2_000,
    };
    let (sessions, provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text(summary_a.clone()),
            CompletionResponse::text("reply after first compact"),
            CompletionResponse::text(summary_b.clone()),
            CompletionResponse::text("reply after second compact"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    let secret_a = format!("SECRET-A-{}", "a".repeat(80));
    seed_turns(
        &sessions,
        id,
        &[
            (&secret_a, "assistant about alpha"),
            ("mid turn", "mid answer"),
            ("tail-1 question", "tail-1 answer"),
        ],
    )
    .await;

    // Compaction 1: elides SECRET-A region; model sees summary A + kept tail.
    let reply1 = sessions.turn(id, "after first compact").await.unwrap();
    assert_eq!(reply1, "reply after first compact");

    // Grow past the post-compact suffix so the next turn has material to roll forward — including
    // a second secret that must only reach the *second* summarizer, not the final turn request.
    let secret_b = format!("SECRET-B-{}", "b".repeat(80));
    seed_turns(
        &sessions,
        id,
        &[
            (&secret_b, "assistant about beta"),
            ("tail-2 question", "tail-2 answer"),
        ],
    )
    .await;

    // Compaction 2: rolling update — prior summary + new secrets go to the summarizer; turn sees B.
    let reply2 = sessions.turn(id, "after second compact").await.unwrap();
    assert_eq!(reply2, "reply after second compact");

    let requests = provider.received_requests();
    assert_eq!(
        requests.len(),
        4,
        "summarizer1 + turn1 + summarizer2 + turn2"
    );

    let summarizer2 = &requests[2];
    let summarizer2_blob: String = summarizer2
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        summarizer2_blob.contains(compaction::SUMMARY_HEADER)
            && summarizer2_blob.contains("SUMMARY-A: code word is ALPHA"),
        "second summarizer must see the prior rolling summary to fold it forward, got:\n{summarizer2_blob}"
    );
    assert!(
        summarizer2_blob.contains("SECRET-B-"),
        "second summarizer must see post-marker secrets being folded in"
    );

    let turn2 = &requests[3];
    assert!(
        turn2.messages.iter().any(|m| m
            .content
            .contains("SUMMARY-B: code word is ALPHA; later topic is BETA")),
        "second compacted view must carry the new rolling summary"
    );
    assert!(
        !turn2
            .messages
            .iter()
            .any(|m| m.content.contains("SECRET-A-") || m.content.contains("SECRET-B-")),
        "neither generation's raw secrets may reach the model after the second compaction"
    );
    // The first summary text itself should have been superseded by summary B in the model view
    // (the old marker is in the elided region / folded into B).
    assert!(
        !turn2
            .messages
            .iter()
            .any(|m| m.content.contains("SUMMARY-A: code word is ALPHA")
                && !m.content.contains("SUMMARY-B")),
        "stale summary A must not ride the post-second-compaction turn as the active marker"
    );
    assert!(
        turn2
            .messages
            .iter()
            .any(|m| m.content == "tail-2 question")
            && turn2
                .messages
                .iter()
                .any(|m| m.content == "after second compact"),
        "kept tail and incoming user message must survive the second compaction"
    );

    // Two markers on the durable transcript (append-only); rendered history keeps both.
    let history = sessions.history(id).await.unwrap();
    let markers = history
        .iter()
        .filter(|m| m.content.starts_with(compaction::SUMMARY_HEADER))
        .count();
    assert_eq!(
        markers, 2,
        "each compaction must leave a durable marker node"
    );
}

/// Store that injects a single `append` failure for a node whose content equals `fail_once_content`,
/// then delegates forever. Used to exercise partial tail re-append after the marker is written.
struct FailOnceContentStore {
    inner: Arc<SessionStore>,
    fail_once_content: std::sync::Mutex<Option<String>>,
    fail_count: AtomicUsize,
}

#[async_trait]
impl ConversationStore for FailOnceContentStore {
    async fn create(
        &self,
        new: liberado_conversation_store::NewConversation,
    ) -> liberado_conversation_store::StoreResult<liberado_conversation_store::ConversationHeader>
    {
        self.inner.create(new).await
    }

    async fn append(
        &self,
        conversation: Ulid,
        node: NewNode,
    ) -> liberado_conversation_store::StoreResult<MessageNode> {
        let should_fail = {
            let mut guard = self.fail_once_content.lock().unwrap();
            if guard.as_ref() == Some(&node.message.content) {
                *guard = None;
                true
            } else {
                false
            }
        };
        if should_fail {
            self.fail_count.fetch_add(1, AtomicOrdering::SeqCst);
            return Err(StoreError::Io(std::io::Error::other(
                "injected tail re-append failure",
            )));
        }
        self.inner.append(conversation, node).await
    }

    async fn leaf_path(
        &self,
        conversation: Ulid,
        leaf: Option<Ulid>,
    ) -> liberado_conversation_store::StoreResult<Vec<MessageNode>> {
        self.inner.leaf_path(conversation, leaf).await
    }

    async fn node(
        &self,
        conversation: Ulid,
        id: Ulid,
    ) -> liberado_conversation_store::StoreResult<Option<MessageNode>> {
        self.inner.node(conversation, id).await
    }

    async fn children(
        &self,
        conversation: Ulid,
        id: Ulid,
    ) -> liberado_conversation_store::StoreResult<Vec<Ulid>> {
        self.inner.children(conversation, id).await
    }

    async fn list(
        &self,
    ) -> liberado_conversation_store::StoreResult<
        Vec<liberado_conversation_store::ConversationHeader>,
    > {
        self.inner.list().await
    }

    async fn header(
        &self,
        conversation: Ulid,
    ) -> liberado_conversation_store::StoreResult<liberado_conversation_store::ConversationHeader>
    {
        self.inner.header(conversation).await
    }

    async fn set_title(
        &self,
        conversation: Ulid,
        title: String,
    ) -> liberado_conversation_store::StoreResult<()> {
        self.inner.set_title(conversation, title).await
    }
}

/// If a tail re-append fails after the marker is durable, this turn must still see the full kept
/// tail (not a truncated in-memory view). Persistence of that one node may still be incomplete —
/// that is the inherent limit without multi-node transactions — but the break-early bug that also
/// stripped remaining tail from *this turn's* conversation is what we guard against.
#[tokio::test]
async fn partial_tail_reappend_failure_keeps_full_view_for_this_turn() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FailOnceContentStore {
        inner: Arc::new(SessionStore::open(dir.path()).await),
        // Armed *after* seed so the original "tail answer" node can land; only the compaction
        // re-append of that content is injected to fail.
        fail_once_content: std::sync::Mutex::new(None),
        fail_count: AtomicUsize::new(0),
    });
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("SUMMARY: partial-tail test"),
            CompletionResponse::text("still answered"),
        ],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store.clone(), executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 1,
            keep_recent_turns: 1,
            ..CompactionConfig::default()
        },
        provider.clone(),
    );
    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[
            ("u1 secret", "a1"),
            ("u2", "a2"),
            ("tail question", "tail answer"),
        ],
    )
    .await;
    // Fail the re-append of the assistant half of the kept tail (keep_recent_turns=1 →
    // user+assistant). Marker and the user half of the re-tail succeed; this one fails once.
    *store.fail_once_content.lock().unwrap() = Some("tail answer".into());

    let reply = sessions.turn(id, "fresh question").await.unwrap();
    assert_eq!(reply, "still answered");
    assert_eq!(
        store.fail_count.load(AtomicOrdering::SeqCst),
        1,
        "the injected failure must have fired exactly once"
    );

    let requests = provider.received_requests();
    assert_eq!(requests.len(), 2, "summarizer + turn");
    let turn_req = &requests[1];
    assert!(
        turn_req
            .messages
            .iter()
            .any(|m| m.content == "tail question")
            && turn_req.messages.iter().any(|m| m.content == "tail answer")
            && turn_req
                .messages
                .iter()
                .any(|m| m.content == "fresh question"),
        "this turn's model view must include the full kept tail even when one re-append failed; got: {:?}",
        turn_req
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        turn_req
            .messages
            .iter()
            .any(|m| m.content.contains("SUMMARY: partial-tail test")),
        "marker must still be in the compacted view"
    );
}

#[tokio::test]
async fn stream_turn_also_compacts() {
    // The streaming path (webui/TUI/CLI) shares `maybe_compact` with `turn` — prove it.
    let dir = tempfile::tempdir().unwrap();
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1, // always fires
        keep_recent_turns: 1,
        ..CompactionConfig::default()
    };
    let (sessions, _provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text("SUMMARY: old stuff"),
            CompletionResponse::text("streamed answer"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[("u1", "a1"), ("u2", "a2"), ("tail q", "tail a")],
    )
    .await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    sessions.turn_stream(id, "stream me", &tx).await.unwrap();
    drop(tx);
    let mut tokens = String::new();
    while let Some(ev) = rx.recv().await {
        if let AgentEvent::Token(t) = ev {
            tokens.push_str(&t);
        }
    }
    assert_eq!(tokens, "streamed answer");
    let history = sessions.history(id).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "the streaming path must persist the marker too"
    );
}

#[tokio::test]
async fn no_compaction_under_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1_000_000,
        ..CompactionConfig::default()
    };
    let (sessions, provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text("r1"),
            CompletionResponse::text("r2"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "first thing").await.unwrap();
    sessions.turn(id, "second thing").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    assert!(
        !history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "no marker without a compaction"
    );
    // Ordinary rehydration: turn 2's request still carries turn 1 verbatim.
    let requests = provider.received_requests();
    assert_eq!(requests.len(), 2, "no summarizer call under the trigger");
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|m| m.content == "first thing"),
        "under-trigger history must ride along untouched"
    );
}

#[tokio::test]
async fn disabled_config_never_compacts() {
    let dir = tempfile::tempdir().unwrap();
    let config = CompactionConfig {
        enabled: false,
        trigger_tokens: 1, // would always fire if enabled
        ..CompactionConfig::default()
    };
    let (sessions, provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text("r1"),
            CompletionResponse::text("r2"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(&sessions, id, &[("u1", "a1"), ("u2", "a2")]).await;
    sessions.turn(id, "third thing").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    assert!(
        !history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "a disabled config must never write markers"
    );
    assert_eq!(provider.received_requests().len(), 1);
    assert!(
        provider.received_requests()[0]
            .messages
            .iter()
            .any(|m| m.content == "u1"),
        "disabled compaction passes history through untouched"
    );
}

/// A provider that fails its first completion (the summarizer) and delegates the rest to an inner
/// mock — the summarizer-failure path must degrade to running the turn uncompacted.
struct FailOnceProvider {
    inner: MockProvider,
    failed: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl Provider for FailOnceProvider {
    fn model(&self) -> String {
        self.inner.model()
    }
    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        if !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(ProviderError::Transport("summarizer boom".into()));
        }
        self.inner.complete(request).await
    }
}

#[tokio::test]
async fn summarizer_failure_runs_the_turn_uncompacted() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(FailOnceProvider {
        inner: MockProvider::with_script("mock", [CompletionResponse::text("uncompacted answer")]),
        failed: std::sync::atomic::AtomicBool::new(false),
    });
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 1, // always fires
            keep_recent_turns: 1,
            ..CompactionConfig::default()
        },
        provider.clone(),
    );
    let id = sessions.create(None).await.unwrap();
    seed_turns(&sessions, id, &[("u1 secret", "a1"), ("u2", "a2")]).await;

    // The turn must SUCCEED despite the summarizer failing — compaction may never cost the human
    // their turn.
    let reply = sessions.turn(id, "still answer me").await.unwrap();
    assert_eq!(reply, "uncompacted answer");

    let history = sessions.history(id).await.unwrap();
    assert!(
        !history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "a failed summarization must not persist a marker"
    );
}

#[tokio::test]
async fn set_compaction_trigger_tokens_updates_live_threshold() {
    // Hot-swap path: boot with a high trigger, then lower it as if a smaller-window model was
    // selected — the next turn must compact under the new threshold.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("SUMMARY: after swap"),
            CompletionResponse::text("post-compact answer"),
        ],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            // High enough that seed turns alone won't fire until we lower the live threshold.
            trigger_tokens: 1_000_000,
            keep_recent_turns: 1,
            ..CompactionConfig::default()
        },
        provider.clone(),
    );
    assert_eq!(sessions.compaction_trigger_tokens(), Some(1_000_000));

    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[
            ("u1 secret-alpha", "a1"),
            ("u2 secret-beta", "a2"),
            ("u3 keep-me", "a3"),
        ],
    )
    .await;

    // Lower the live threshold as select_model / resync_compaction_trigger_for_face_model does.
    sessions.set_compaction_trigger_tokens(1);
    assert_eq!(sessions.compaction_trigger_tokens(), Some(1));

    let reply = sessions.turn(id, "after swap").await.unwrap();
    assert_eq!(reply, "post-compact answer");

    let history = sessions.history(id).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "after lowering the live trigger, the next turn must compact"
    );
}

/// Compaction re-appends the kept tail so the model view is a contiguous log suffix. Those copies
/// must not surface to readers that walk the raw leaf path, or every compaction repeats the last
/// `keep_recent_turns` turns in rendered history and shifts `Author::User` turn indices (fork /
/// rewind resolves "turn N" against that count).
#[tokio::test]
async fn compaction_tail_copies_are_not_visible_in_rendered_history() {
    let dir = tempfile::tempdir().unwrap();
    let summary = "SUMMARY: rolled up".to_string();
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1, // always fire
        keep_recent_turns: 1,
        summary_max_tokens: 512,
        tool_result_max_chars: 2_000,
    };
    let (sessions, _provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text(summary.clone()),
            CompletionResponse::text("fresh answer"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(&sessions, id, &[("u-one", "a-one"), ("TAILMARK", "a-tail")]).await;

    let user_turns_before = sessions
        .history(id)
        .await
        .unwrap()
        .iter()
        .filter(|m| m.role == Role::User)
        .count();

    sessions.turn(id, "fresh question").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    assert_eq!(
        history.iter().filter(|m| m.content == "TAILMARK").count(),
        1,
        "the kept tail must appear once in rendered history, not once per compaction"
    );
    // Compaction never deletes: the elided originals and the marker are both still rendered.
    assert!(
        history.iter().any(|m| m.content == "u-one"),
        "elided originals must still render in full history"
    );
    assert!(
        history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "the marker must render as a checkpoint bubble"
    );
    // Turn indexing (fork/rewind counts `Author::User` nodes) gained exactly the new turn.
    let user_turns_after = history.iter().filter(|m| m.role == Role::User).count();
    assert_eq!(
        user_turns_after,
        user_turns_before + 1,
        "compaction must not inflate the user-turn count that fork/rewind indexes against"
    );
}
