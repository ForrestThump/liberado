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

/// A provider that answers, but not immediately.
///
/// The delay is the point: with an instant provider a turn finishes before anyone can stop watching
/// it, so a test that drops its receiver and then finds the reply on disk proves nothing — it would
/// pass just as well against the old connection-owned turn. This makes "the watcher left while the
/// turn was still running" an actual state the test passes through.
struct SlowProvider {
    delay: std::time::Duration,
    reply: String,
}
#[async_trait]
impl Provider for SlowProvider {
    fn model(&self) -> String {
        "slow".into()
    }
    async fn complete(&self, _request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        tokio::time::sleep(self.delay).await;
        Ok(CompletionResponse::text(&self.reply))
    }
}

/// A `ChatSessions` whose provider takes `delay` to answer. See [`SlowProvider`].
async fn slow_sessions_at(
    root: &std::path::Path,
    delay: std::time::Duration,
    reply: &str,
) -> ChatSessions {
    let store = Arc::new(SessionStore::open(root).await);
    let provider = Arc::new(SlowProvider {
        delay,
        reply: reply.to_string(),
    });
    let executor = Executor::new(provider, Budget::default());
    ChatSessions::new(store, executor, Arc::new(NoTools))
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

/// A turn the client abandons keeps the question and drops the half-answer.
///
/// This asserted "persists nothing" until 2026-08-01, when that cost a real conversation: switching
/// WebUI tabs unmounts the chat component, which closes the `EventSource`, which drops the turn —
/// and the user's message went with it, leaving a titled conversation containing only a system
/// prompt. The reply must still not persist; a partial answer is the thing the rule exists to
/// prevent. The question is not.
#[tokio::test]
async fn cancelled_stream_keeps_the_user_message_and_no_reply() {
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

    let history = sessions.history(id).await.unwrap();
    assert_eq!(
        history.iter().map(|m| m.role).collect::<Vec<_>>(),
        vec![Role::System, Role::User],
        "an abandoned turn must keep exactly the system prompt and the question"
    );
    assert_eq!(history[1].content, "hi");
    assert!(
        !history.iter().any(|m| m.role == Role::Assistant),
        "no part of an unfinished reply may be persisted"
    );
}

/// The message is durable *before* the model is called, not merely by the time the turn ends —
/// otherwise a client that leaves during a slow first token still loses it. `PendingProvider` never
/// answers, so reaching a persisted user node proves the write happened ahead of inference.
#[tokio::test]
async fn the_user_message_is_durable_before_the_provider_answers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools));

    let id = sessions.create(None).await.unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(8);

    let fut = sessions.turn_stream(id, "still in flight", &tx);
    tokio::pin!(fut);
    assert!(futures::poll!(fut.as_mut()).is_pending());

    // Read while the turn is *still running* — the future above is deliberately not dropped.
    let history = sessions.history(id).await.unwrap();
    assert!(
        history.iter().any(|m| m.content == "still in flight"),
        "the question must be on disk before the answer exists, not after"
    );
}

/// A completed turn writes the user message exactly once. The up-front write and the post-turn tail
/// are two different code paths appending to one log, which is precisely how a message gets stored
/// twice.
#[tokio::test]
async fn a_successful_turn_does_not_duplicate_the_user_message() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("sure")]).await;

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "only once please").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|m| m.content == "only once please")
            .count(),
        1,
        "the user message was persisted twice"
    );
    assert_eq!(
        history.iter().map(|m| m.role).collect::<Vec<_>>(),
        vec![Role::System, Role::User, Role::Assistant],
        "a normal turn's shape must be unchanged by the early write"
    );
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
            ephemeral: false,
            visibility: Default::default(),
            grant: Default::default(),
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
                model: None,
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
                        model: None,
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
        ..CompactionConfig::default()
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
        ..CompactionConfig::default()
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

    async fn set_grant(
        &self,
        conversation: Ulid,
        grant: liberado_session::SessionGrant,
    ) -> liberado_conversation_store::StoreResult<()> {
        self.inner.set_grant(conversation, grant).await
    }

    async fn delete(&self, conversation: Ulid) -> liberado_conversation_store::StoreResult<()> {
        self.inner.delete(conversation).await
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

    // Lower the live *default* threshold as resync_compaction_trigger_for_face_model does.
    // This conversation has no model of its own, so it observes the new default.
    sessions.set_compaction_trigger_tokens(1);
    assert_eq!(sessions.compaction_trigger_tokens(), Some(1));
    assert_eq!(sessions.compaction_trigger_for_session(id).await, Some(1));

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

/// Two conversations on models with different absolute triggers compact at different points.
///
/// Drives a real `ChatSessions` + durable store. The fixture deliberately sets per-model triggers
/// (as the server does at boot from window sizes) — a single shared `trigger_tokens` cannot pass.
#[tokio::test]
async fn two_conversations_on_different_models_compact_at_different_thresholds() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    // Enough scripted replies: seed turns for two chats + compact summarizer + post-compact for
    // the small-model chat; big-model chat only needs its final turn reply (no summarizer).
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            // seed small: 2 turns
            CompletionResponse::text("s1"),
            CompletionResponse::text("s2"),
            // seed big: 2 turns
            CompletionResponse::text("b1"),
            CompletionResponse::text("b2"),
            // small: summarizer + turn reply
            CompletionResponse::text("SUMMARY: small-window rolled up"),
            CompletionResponse::text("small-model answer"),
            // big: turn only (under its higher trigger)
            CompletionResponse::text("big-model answer"),
        ],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());

    // Per-model thresholds as server would pre-resolve from [[models]] windows.
    // Seed with a high default so history builds without compacting; then pin models.
    let mut model_triggers = std::collections::HashMap::new();
    model_triggers.insert("model-64k".into(), 1u32); // always fire once selected
    model_triggers.insert("model-200k".into(), 1_000_000u32); // never fire on this fixture
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 1_000_000, // daemon default: high while seeding
            model_trigger_tokens: model_triggers,
            unknown_model_trigger_tokens: 1_000_000,
            keep_recent_turns: 1,
            ..CompactionConfig::default()
        },
        provider.clone(),
    );

    let small = sessions.create(None).await.unwrap();
    let big = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        small,
        &[("u1 secret-alpha", "s1"), ("u2 keep", "s2")],
    )
    .await;
    seed_turns(
        &sessions,
        big,
        &[("u1 secret-alpha", "b1"), ("u2 keep", "b2")],
    )
    .await;

    // Pin models after seed so the compact decision uses per-model thresholds.
    sessions.select_model(small, "model-64k".into());
    sessions.select_model(big, "model-200k".into());

    assert_eq!(
        sessions.compaction_trigger_for_session(small).await,
        Some(1),
        "64k model must resolve to its own low trigger"
    );
    assert_eq!(
        sessions.compaction_trigger_for_session(big).await,
        Some(1_000_000),
        "200k model must resolve to its own high trigger"
    );
    assert_ne!(
        sessions.compaction_trigger_for_session(small).await,
        sessions.compaction_trigger_for_session(big).await,
        "two models must not share one threshold"
    );

    sessions.turn(small, "after pin small").await.unwrap();
    sessions.turn(big, "after pin big").await.unwrap();

    let hist_small = sessions.history(small).await.unwrap();
    let hist_big = sessions.history(big).await.unwrap();
    assert!(
        hist_small
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "low-threshold conversation must compact; history={hist_small:?}"
    );
    assert!(
        !hist_big
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "high-threshold conversation must NOT compact on the same history size; history={hist_big:?}"
    );
    // Elided secret must leave the small model's next request, not the big one's raw path.
    assert!(
        !hist_small
            .iter()
            .any(|m| m.content.contains("secret-alpha"))
            || hist_small
                .iter()
                .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "small chat compacted (marker present)"
    );
}

/// Conversations with no model of their own use the daemon-default trigger (pre–per-conversation
/// model behaviour).
#[tokio::test]
async fn conversation_without_model_uses_daemon_default_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("ok")],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let mut model_triggers = std::collections::HashMap::new();
    model_triggers.insert("pinned".into(), 42u32);
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 12_345,
            model_trigger_tokens: model_triggers,
            unknown_model_trigger_tokens: 99,
            ..CompactionConfig::default()
        },
        provider,
    );

    let unpinned = sessions.create(None).await.unwrap();
    assert_eq!(
        sessions.compaction_trigger_for_session(unpinned).await,
        Some(12_345),
        "no model → daemon default"
    );

    let pinned = sessions.create(None).await.unwrap();
    sessions.select_model(pinned, "pinned".into());
    assert_eq!(
        sessions.compaction_trigger_for_session(pinned).await,
        Some(42),
        "pending per-conversation model → table entry"
    );
}

/// The assertion that would have caught the bug: daemon-wide face-model resync must not retune a
/// conversation that has its own model.
#[tokio::test]
async fn daemon_wide_resync_does_not_retune_conversation_with_own_model() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("seed"),
            CompletionResponse::text("still-here"),
        ],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let mut model_triggers = std::collections::HashMap::new();
    model_triggers.insert("conv-model".into(), 7_777u32);
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 48_000,
            model_trigger_tokens: model_triggers,
            unknown_model_trigger_tokens: 48_000,
            ..CompactionConfig::default()
        },
        provider,
    );

    let pinned = sessions.create(None).await.unwrap();
    // Stamp the model on the log so resolution is durable, not only pending.
    sessions.select_model(pinned, "conv-model".into());
    sessions.turn(pinned, "hello").await.unwrap();
    assert_eq!(
        sessions.compaction_trigger_for_session(pinned).await,
        Some(7_777)
    );

    let unpinned = sessions.create(None).await.unwrap();
    assert_eq!(
        sessions.compaction_trigger_for_session(unpinned).await,
        Some(48_000)
    );

    // Simulate resync_compaction_trigger_for_face_model after POST /api/models/select (daemon-wide).
    sessions.set_compaction_trigger_tokens(1_111);
    assert_eq!(
        sessions.compaction_trigger_tokens(),
        Some(1_111),
        "default updates"
    );
    assert_eq!(
        sessions.compaction_trigger_for_session(unpinned).await,
        Some(1_111),
        "unpinned chats follow the new daemon default"
    );
    assert_eq!(
        sessions.compaction_trigger_for_session(pinned).await,
        Some(7_777),
        "pinned conversation must keep its model trigger after daemon-wide resync — \
         this is the assertion that would have caught the shared-number bug"
    );
}

/// §1 wiring: effective trigger for a session must come from the per-model table path
/// (`CompactionTriggerTable::for_model`), not only from the daemon default. If `maybe_compact` /
/// `compaction_trigger_for_session` were changed to always read `default`, this fails.
#[tokio::test]
async fn per_conversation_trigger_resolution_is_wired_not_only_default() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("x")],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let mut model_triggers = std::collections::HashMap::new();
    // Distinct from default so a default-only path cannot accidentally pass.
    model_triggers.insert("wired-model".into(), 55_555u32);
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 11_111,
            model_trigger_tokens: model_triggers,
            unknown_model_trigger_tokens: 22_222,
            ..CompactionConfig::default()
        },
        provider,
    );

    let id = sessions.create(None).await.unwrap();
    sessions.select_model(id, "wired-model".into());
    let effective = sessions
        .compaction_trigger_for_session(id)
        .await
        .expect("compaction wired");
    assert_eq!(
        effective, 55_555,
        "must use model_trigger_tokens[wired-model], not default 11111 — \
         deleting per-conversation resolution fails this test"
    );
    assert_ne!(effective, sessions.compaction_trigger_tokens().unwrap());
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
        ..CompactionConfig::default()
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

// ── Per-session grants (session profiles, step 2) ────────────────────────────────────────────
//
// `session_capabilities` decides what authority a turn runs under. Getting it wrong is invisible:
// too narrow silently removes tools from working chats, too wide silently ignores a profile. Both
// look like a normal turn.

/// The migration case, and the one that would have broken every existing conversation. Chats created
/// before profiles carry an empty default grant; reading that literally would leave them with no
/// tools at all.
#[tokio::test]
async fn a_conversation_with_no_profile_runs_under_the_process_grant() {
    let dir = tempfile::tempdir().unwrap();
    let process_grant =
        CapabilitySet::from_iter([liberado_common::Capability::ExecuteMcp("tasks-mcp".into())]);
    let sessions = sessions_at(dir.path(), vec![]).await.with_guards(
        Vec::new(),
        process_grant.clone(),
        PathBuf::new(),
        ProposalSigner::random(),
    );

    let id = sessions.create(None).await.unwrap();

    assert_eq!(
        sessions.session_capabilities(id).await,
        process_grant,
        "an unprofiled chat must keep the daemon's grant, not inherit the store's empty default"
    );
}

/// A named profile is the session's authority, replacing the process grant rather than intersecting
/// with it — otherwise a profile could never add an MCP the face agent's own grant lacks, which is
/// most of the point.
#[tokio::test]
async fn a_named_profile_replaces_the_process_grant() {
    let dir = tempfile::tempdir().unwrap();
    let process_grant =
        CapabilitySet::from_iter([liberado_common::Capability::ExecuteMcp("tasks-mcp".into())]);
    let sessions = sessions_at(dir.path(), vec![]).await.with_guards(
        Vec::new(),
        process_grant,
        PathBuf::new(),
        ProposalSigner::random(),
    );

    let profile = SessionGrant {
        capabilities: CapabilitySet::from_iter([
            liberado_common::Capability::ExecuteMcp("spider-mcp".into()),
            liberado_common::Capability::ExecuteTool("turbovault:read_note".into()),
        ]),
        profile: Some("basic-chat".into()),
        overrides: serde_json::Value::Null,
        ..Default::default()
    };
    let id = sessions
        .create_with_grant(None, profile.clone())
        .await
        .unwrap();

    let effective = sessions.session_capabilities(id).await;
    assert!(effective.grants_tool("spider-mcp:fetch"));
    assert!(effective.grants_tool("turbovault:read_note"));
    assert!(
        !effective.grants_tool("turbovault:write_note"),
        "the per-tool half of the profile must survive the round trip through the store"
    );
    assert!(
        !effective.grants_tool("tasks-mcp:add"),
        "the profile replaces the process grant; it does not add to it"
    );
}

/// "This chat may call nothing" has to be sayable, and distinguishable from "no profile chosen".
/// The profile *name* is what carries that intent — an empty capability set alone cannot.
#[tokio::test]
async fn a_named_profile_granting_nothing_is_honored_not_treated_as_unset() {
    let dir = tempfile::tempdir().unwrap();
    let process_grant =
        CapabilitySet::from_iter([liberado_common::Capability::ExecuteMcp("tasks-mcp".into())]);
    let sessions = sessions_at(dir.path(), vec![]).await.with_guards(
        Vec::new(),
        process_grant,
        PathBuf::new(),
        ProposalSigner::random(),
    );

    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                capabilities: CapabilitySet::empty(),
                profile: Some("no-tools".into()),
                overrides: serde_json::Value::Null,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        sessions.session_capabilities(id).await,
        CapabilitySet::empty()
    );
}

/// The grant is on the header line, so it must survive a restart like any other session state.
#[tokio::test]
async fn a_profile_survives_reopening_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let grant = SessionGrant {
        capabilities: CapabilitySet::from_iter([liberado_common::Capability::ExecuteTool(
            "turbovault:read_note".into(),
        )]),
        profile: Some("basic-chat".into()),
        overrides: serde_json::Value::Null,
        ..Default::default()
    };

    let id = {
        let sessions = sessions_at(dir.path(), vec![]).await;
        sessions.create_with_grant(None, grant).await.unwrap()
    };

    let reopened = sessions_at(dir.path(), vec![]).await;
    let header = reopened.store.header(id).await.unwrap();
    assert_eq!(header.grant.profile.as_deref(), Some("basic-chat"));
    assert!(
        header
            .grant
            .capabilities
            .grants_tool("turbovault:read_note")
    );
}

/// A lookup failure must not quietly become an authority change in either direction.
#[tokio::test]
async fn an_unknown_session_falls_back_to_the_process_grant() {
    let dir = tempfile::tempdir().unwrap();
    let process_grant =
        CapabilitySet::from_iter([liberado_common::Capability::ExecuteMcp("tasks-mcp".into())]);
    let sessions = sessions_at(dir.path(), vec![]).await.with_guards(
        Vec::new(),
        process_grant.clone(),
        PathBuf::new(),
        ProposalSigner::random(),
    );

    assert_eq!(
        sessions.session_capabilities(Ulid::new()).await,
        process_grant
    );
}

/// A profile that turns dispatch off must actually change the *shape* of the turn, not merely
/// shorten the tool list â€” that is most of what "basic chat" means.
#[tokio::test]
async fn a_profile_can_switch_delegation_off_for_one_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![]).await;

    let default_on = sessions.create(None).await.unwrap();
    let basic = sessions
        .create_with_grant(
            None,
            SessionGrant {
                capabilities: CapabilitySet::empty(),
                profile: Some("basic-chat".into()),
                delegation: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert!(
        sessions.turn_settings(default_on).await.delegation == sessions.delegation_mode_for_test(),
        "no profile must inherit the daemon's setting, whatever it is"
    );
    assert!(
        !sessions.turn_settings(basic).await.delegation,
        "the profile's `delegation = false` must win for this conversation"
    );
}

/// `None` means "inherit", not "off" â€” a profile that says nothing about delegation must not
/// silently disable it.
#[tokio::test]
async fn a_profile_silent_on_delegation_inherits_the_daemon_setting() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![]).await;
    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                profile: Some("quiet".into()),
                delegation: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        sessions.turn_settings(id).await.delegation,
        sessions.delegation_mode_for_test()
    );
}

#[tokio::test]
async fn a_profiles_prompt_append_reaches_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![]).await;
    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                profile: Some("terse".into()),
                prompt_append: Some("Answer in one sentence.".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        sessions.turn_settings(id).await.prompt_append.as_deref(),
        Some("Answer in one sentence.")
    );
    // ...and an unprofiled chat gets none, rather than inheriting another session's.
    let plain = sessions.create(None).await.unwrap();
    assert!(sessions.turn_settings(plain).await.prompt_append.is_none());
}

/// The nudge must qualify the system prompt, not arrive as if the user said it â€” a model treats
/// those very differently.
#[test]
fn a_prompt_append_lands_after_the_system_prompt_and_before_the_first_user_turn() {
    let mut convo = Conversation::from_history(vec![
        Message::system("base prompt"),
        Message::user("hello"),
        Message::assistant("hi"),
    ]);
    convo.apply_prompt_append(Some("Be terse."));

    let roles: Vec<Role> = convo.messages_for_test().iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![Role::System, Role::System, Role::User, Role::Assistant],
        "the nudge must sit with the system prompt, not among the dialogue"
    );
    assert_eq!(convo.messages_for_test()[1].content, "Be terse.");
}

#[test]
fn an_absent_or_blank_prompt_append_changes_nothing() {
    for extra in [None, Some(""), Some("   \n ")] {
        let mut convo =
            Conversation::from_history(vec![Message::system("base"), Message::user("q")]);
        convo.apply_prompt_append(extra);
        assert_eq!(
            convo.messages_for_test().len(),
            2,
            "blank nudge must not add a message: {extra:?}"
        );
    }
}

// ── The prompt must follow the profile ───────────────────────────────────────────────────────────
//
// Found live on 2026-07-28, not by CI: a `basic-chat` session (delegation off, five real tools, no
// `delegate`) was still handed the face-agent root prompt — "you are a face agent, not a tool user…
// call the `delegate` tool", plus an instruction not to enumerate its own tools. Asked for its open
// tasks it answered "I'll fetch your open tasks first." and called nothing. The prompt and the tool
// surface were two sources of truth and they drifted the moment step 5 made the surface per-session
// while the prompt stayed daemon-wide.

/// The regression test that matters: assert on what the **provider actually received**, not on the
/// helper that built it. A session that does not delegate must not be told to delegate.
#[tokio::test]
async fn a_non_delegating_session_is_not_told_it_is_a_face_agent() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text("ok")],
    ));
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(provider.clone(), Budget::default());
    // Delegation mode on, so the *persisted root prompt* is the face-agent one — exactly the live
    // configuration. No hub attached, so this turn does not run as the face agent.
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_delegation_mode(true);

    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                profile: Some("basic-chat".into()),
                delegation: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sessions
        .turn(id, "What tasks do I have open?")
        .await
        .unwrap();

    let sent = &provider.received_requests()[0].messages[0];
    assert_eq!(sent.role, Role::System);
    assert_ne!(
        sent.content, HUMAN_INTERFACE_SYSTEM_PROMPT,
        "a session that cannot delegate must not be handed the face-agent prompt"
    );
    assert!(
        !sent.content.contains("delegate"),
        "the model must not be instructed to call a tool it does not hold; got: {}",
        &sent.content[..sent.content.len().min(200)]
    );
}

/// The counterpart: a session that *does* delegate must keep the face-agent prompt. A fix that
/// stripped it unconditionally would trade one drift for another.
#[tokio::test]
async fn a_delegating_session_keeps_the_face_agent_prompt() {
    use liberado_session::{GoalSessionHub, GoalSessionStore};
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text("ok")],
    ));
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools))
        .with_delegation_mode(true)
        .with_goal_hub(Arc::new(GoalSessionHub::new(GoalSessionStore::new())));

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "hello").await.unwrap();

    assert_eq!(
        provider.received_requests()[0].messages[0].content,
        HUMAN_INTERFACE_SYSTEM_PROMPT,
        "the face agent must still be told it is one"
    );
}

#[test]
fn the_swap_replaces_the_builtin_face_prompt_only() {
    // The built-in face prompt is swapped...
    let mut convo = Conversation::from_history(vec![
        Message::system(HUMAN_INTERFACE_SYSTEM_PROMPT),
        Message::user("q"),
    ]);
    convo.apply_direct_agent_prompt();
    assert_eq!(convo.messages_for_test()[0].content, DEFAULT_SYSTEM_PROMPT);

    // ...an operator's own prompt is not. They chose that text for every session, and discarding it
    // silently would be the same class of bug pointing the other way.
    let custom = "You are a narrow research assistant. Never speculate.";
    let mut convo = Conversation::from_history(vec![Message::system(custom), Message::user("q")]);
    convo.apply_direct_agent_prompt();
    assert_eq!(convo.messages_for_test()[0].content, custom);

    // ...and a prompt already correct for this path is left exactly as it is.
    let mut convo = Conversation::from_history(vec![
        Message::system(DEFAULT_SYSTEM_PROMPT),
        Message::user("q"),
    ]);
    convo.apply_direct_agent_prompt();
    assert_eq!(convo.messages_for_test()[0].content, DEFAULT_SYSTEM_PROMPT);
}

/// Order is load-bearing: the profile's nudge qualifies whichever base prompt ends up in force, so
/// it has to stay last. Swapping the base after appending would put them the wrong way round.
#[test]
fn the_swap_leaves_the_profile_nudge_after_the_base_prompt() {
    let mut convo = Conversation::from_history(vec![
        Message::system(HUMAN_INTERFACE_SYSTEM_PROMPT),
        Message::user("q"),
    ]);
    convo.apply_direct_agent_prompt();
    convo.apply_prompt_append(Some("Answer directly and briefly."));

    let msgs = convo.messages_for_test();
    assert_eq!(msgs[0].content, DEFAULT_SYSTEM_PROMPT);
    assert_eq!(msgs[1].content, "Answer directly and briefly.");
    assert_eq!(msgs[2].role, Role::User);
}

#[test]
fn the_swap_is_a_no_op_on_an_empty_or_headless_history() {
    let mut empty = Conversation::from_history(vec![]);
    empty.apply_direct_agent_prompt();
    assert!(empty.messages_for_test().is_empty());

    // A history whose first message is not a system prompt must not be rewritten into one.
    let mut headless = Conversation::from_history(vec![Message::user("q")]);
    headless.apply_direct_agent_prompt();
    assert_eq!(headless.messages_for_test()[0].role, Role::User);
    assert_eq!(headless.messages_for_test()[0].content, "q");
}

// ── The tool manifest: one value, two renderings ────────────────────────────────────────────────

/// The property the whole design rests on: the tools **named in the prompt** and the tools **sent in
/// the request** are the same list, because both come off the runtime handed to the executor.
///
/// Asserted as an equality between the two, not as "the prompt mentions calendar-mcp:list" — a
/// substring check would still pass if the prompt named a tool the request omitted, which is exactly
/// vtcode's `prompts.coder` naming `write_file` against a `unified_file` toolset.
#[tokio::test]
async fn the_prompt_names_exactly_the_tools_the_request_carries() {
    use liberado_common::{Capability, CapabilitySet};

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("ok")],
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
    sessions.turn(id, "what's on my calendar?").await.unwrap();

    let request = &provider.received_requests()[0];
    let carried: Vec<String> = request.tools.iter().map(|t| t.name.clone()).collect();
    assert!(
        !carried.is_empty(),
        "fixture should carry at least one tool"
    );

    let manifest = request
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .find(|c| c.contains("available to you on this turn"))
        .expect("the turn must state which tools it holds");

    for name in &carried {
        assert!(
            manifest.contains(name.as_str()),
            "tool {name} is in the request but missing from the prompt: {manifest}"
        );
    }
    assert!(
        !manifest.contains("write_file"),
        "sanity: the manifest must not invent tools the request does not carry"
    );
}

/// A turn with nothing to call must say so outright. Otherwise the model fills the silence by
/// offering to look something up — the announce-then-stall failure, reached from the other side.
#[test]
fn a_toolless_turn_is_told_not_to_offer_lookups() {
    let mut convo = Conversation::from_history(vec![Message::system("base"), Message::user("q")]);
    convo.apply_available_tools(&[]);
    let stated = &convo.messages_for_test()[1].content;
    assert!(stated.contains("no tools"), "got: {stated}");
    assert!(
        stated.contains("cannot"),
        "an empty manifest must forbid promising a lookup, not merely omit tools: {stated}"
    );
    // Measured live 2026-08-01. Told it had no tools "on this turn", the model deferred instead —
    // "ask me again on the next turn and I'll do a fresh lookup" — which was untrue: the profile
    // lacked the tool entirely, so no later turn would have differed. Accurate about the turn,
    // misleading about the future, and the same announce-then-cannot shape as the original bug.
    assert!(
        stated.contains("asking again later"),
        "an empty manifest must not invite a retry it cannot honour: {stated}"
    );
    // ...while still allowing honest use of what is already in the conversation, which is what the
    // model got right unprompted: it cited the earlier result and labelled it as earlier.
    assert!(stated.contains("not current"), "{stated}");
}

/// It has to beat concrete tool successes sitting further up the transcript, so it goes last —
/// after the profile nudge, immediately before the dialogue.
#[test]
fn the_tool_manifest_is_the_last_word_before_the_dialogue() {
    let mut convo = Conversation::from_history(vec![
        Message::system(HUMAN_INTERFACE_SYSTEM_PROMPT),
        Message::user("earlier"),
        Message::assistant("earlier reply"),
    ]);
    convo.apply_direct_agent_prompt();
    convo.apply_prompt_append(Some("Answer directly and briefly."));
    convo.apply_available_tools(&[ToolDef::new(
        "turbovault:tasks_list",
        "list tasks",
        serde_json::json!({ "type": "object" }),
    )]);

    let msgs = convo.messages_for_test();
    assert_eq!(msgs[0].content, DEFAULT_SYSTEM_PROMPT);
    assert_eq!(msgs[1].content, "Answer directly and briefly.");
    assert!(msgs[2].content.contains("turbovault:tasks_list"));
    assert_eq!(msgs[2].role, Role::System);
    assert_eq!(
        msgs[3].role,
        Role::User,
        "the manifest must be the final system message, not buried among the dialogue"
    );
}

/// The stale-evidence case: a transcript containing a successful call to a since-revoked tool must
/// be explicitly outranked, not merely contradicted by omission.
#[test]
fn the_manifest_tells_the_model_to_distrust_the_transcript() {
    let mut convo = Conversation::from_history(vec![Message::system("base"), Message::user("q")]);
    convo.apply_available_tools(&[ToolDef::new(
        "search",
        "search",
        serde_json::json!({ "type": "object" }),
    )]);
    let stated = &convo.messages_for_test()[1].content;
    assert!(
        stated.contains("withdrawn") && stated.contains("trust this list"),
        "a tool absent here but present in history must be addressed head-on: {stated}"
    );
}

/// Transient system messages are injected at the *front* of the view, so slicing the turn's output
/// by a pre-turn length walks back into history and re-persists messages already on disk.
///
/// Latent for as long as the only injector was a profile's optional nudge; the tool manifest runs
/// every turn, which made it certain. Caught by an unrelated compaction test starting to fail —
/// duplicated messages inflated the next load past the compaction trigger.
#[tokio::test]
async fn a_turn_persists_only_its_own_messages_not_the_injected_ones() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(
        dir.path(),
        vec![
            CompletionResponse::text("first answer"),
            CompletionResponse::text("second answer"),
        ],
    )
    .await;
    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                profile: Some("terse".into()),
                prompt_append: Some("Be terse.".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    sessions.turn(id, "first question").await.unwrap();
    sessions.turn(id, "second question").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    for probe in ["first question", "first answer", "second question"] {
        assert_eq!(
            history.iter().filter(|m| m.content == probe).count(),
            1,
            "{probe:?} must be stored exactly once; history: {:?}",
            history.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }
    // And the per-turn injections are views, never records.
    for injected in ["Be terse.", "available to you on this turn"] {
        assert!(
            !history.iter().any(|m| m.content.contains(injected)),
            "{injected:?} is a per-turn view and must not be persisted"
        );
    }
}

/// A profile's `mcps` must reach the **non-delegating** path — the one `delegation = false` selects,
/// and therefore the only path a "basic chat" profile ever runs on.
///
/// It did not. `build_turn_runtime` scoped and gated against the process-wide grant, so a profile's
/// tools resolved into the session header, showed up over the API, and then surfaced as nothing:
/// `main-agent` deliberately holds no `ExecuteMcp` ("specialists stay on dispatcher"), so the
/// intersection was always empty. Live, the model correctly reported it had no access to a tool the
/// grant plainly listed.
#[tokio::test]
async fn a_profiles_tools_reach_a_non_delegating_turn() {
    use liberado_common::{Capability, CapabilitySet};

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("ok")],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    // The process grant holds nothing executable — exactly like the live `main-agent` grant.
    let sessions = ChatSessions::new(store, executor, Arc::new(OneTool("turbovault:tasks_list")))
        .with_guards(
            vec![("turbovault".into(), Consequence::Reversible)],
            CapabilitySet::empty(),
            dir.path().join("proposals"),
            ProposalSigner::random(),
        );

    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                capabilities: CapabilitySet::from_iter([Capability::ExecuteTool(
                    "turbovault:tasks_list".into(),
                )]),
                profile: Some("basic-chat".into()),
                delegation: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sessions.turn(id, "what tasks are open?").await.unwrap();

    let offered: Vec<String> = provider.received_requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(
        offered.contains(&"turbovault:tasks_list".to_string()),
        "the profile's tool must be surfaced on the path its own `delegation = false` selects; \
         got {offered:?}"
    );
}

/// The other direction: a session that names no profile must still see the process grant, so this
/// cannot become a migration that silently strips tools from every pre-existing chat.
#[tokio::test]
async fn an_unprofiled_turn_still_sees_the_process_grant() {
    use liberado_common::{Capability, CapabilitySet};

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("ok")],
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
    sessions.turn(id, "what's on my calendar?").await.unwrap();

    let offered: Vec<String> = provider.received_requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(
        offered.contains(&"calendar-mcp:list".to_string()),
        "an unprofiled chat must keep the process grant; got {offered:?}"
    );
}

// ── Per-conversation model: recorded on the log, derived back from it ────────

/// The stamp lands on the turn's nodes, so the log says which model answered.
#[tokio::test]
async fn a_turn_records_the_model_it_ran_on() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("hi")]).await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "hello").await.unwrap();

    let nodes = sessions.store.leaf_path(id, None).await.unwrap();
    let stamped: Vec<_> = nodes
        .iter()
        .filter_map(|n| n.model.as_deref().map(|m| (n.author.clone(), m)))
        .collect();
    assert_eq!(
        stamped,
        vec![(Author::User, "mock"), (Author::Assistant, "mock")],
        "both the question's model and the answer's should be on the log, and nothing else's"
    );
    // The system prompt is nobody's model.
    assert!(nodes[0].model.is_none());
}

/// The point of recording it: the next turn goes to the same model without anything storing a
/// "selected model" field that could disagree with what ran.
#[tokio::test]
async fn the_next_turn_follows_the_model_already_on_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("ok")]).await;
    let id = sessions.create(None).await.unwrap();

    sessions.select_model(id, "chosen-model".into());
    assert_eq!(
        sessions.turn_settings(id).await.model.as_deref(),
        Some("chosen-model"),
        "the pending pick must win for the turn that follows it"
    );

    // Consumed: a second read has nothing pending and falls through to the log, which is still
    // empty of model stamps, so it lands on the provider default.
    assert_eq!(sessions.turn_settings(id).await.model, None);

    // Run a turn, and the log becomes the source.
    sessions.turn(id, "hello").await.unwrap();
    assert_eq!(
        sessions.turn_settings(id).await.model.as_deref(),
        Some("mock"),
        "with history, the conversation stays on whatever last answered it"
    );
}

/// A tool result is produced by an MCP, not a model. Stamping it would make the derivation report a
/// model for a turn no model spoke in.
#[tokio::test]
async fn tool_results_carry_no_model() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("done")]).await;
    let id = sessions.create(None).await.unwrap();

    // Append a tool node directly — the shape a tool-calling turn leaves behind.
    let parent = sessions
        .store
        .leaf_path(id, None)
        .await
        .unwrap()
        .last()
        .map(|n| n.id);
    sessions
        .store
        .append(
            id,
            NewNode {
                parent_id: parent,
                author: Author::Tool,
                message: Message::tool_result("call-1", "result"),
                model: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        sessions.turn_settings(id).await.model,
        None,
        "a tool node must not be read back as the conversation's model"
    );
}

/// Derivation keys on `Author`, not `message.role`. A subagent handoff is authored `goal-session`
/// with an assistant-role body; reading by role would migrate the conversation onto whatever model
/// a delegation happened to use.
#[tokio::test]
async fn a_subagent_handoff_does_not_capture_the_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("ok")]).await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "hello").await.unwrap();

    // A goal-session note lands after the turn, carrying an assistant-role body.
    sessions
        .append_note(id, "the specialist finished")
        .await
        .unwrap();

    assert_eq!(
        sessions.turn_settings(id).await.model.as_deref(),
        Some("mock"),
        "the last *assistant-authored* model still decides, not the note that followed it"
    );
}

/// A conversation whose log predates this field has no stamp anywhere, and must fall back to the
/// provider default rather than failing or inventing one.
#[tokio::test]
async fn a_conversation_with_no_stamps_falls_back_to_the_provider() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), Vec::new()).await;
    let id = sessions.create(None).await.unwrap();
    assert_eq!(sessions.turn_settings(id).await.model, None);
}

// ── Durable turns: a turn outlives the connection watching it ────────────────────────────────

/// The point of the whole change. Start a turn, drop every watcher immediately, and the reply must
/// still be on disk afterwards.
///
/// Before this, the turn was owned by the HTTP response: dropping the stream cancelled the inference
/// and rolled the turn back, so a refresh mid-answer cost the answer. The assertion is on the
/// **persisted node**, not on an event — an event saying it finished is exactly what the old code
/// also managed to not produce.
#[tokio::test]
async fn a_turn_survives_every_watcher_leaving() {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(slow_sessions_at(dir.path(), std::time::Duration::from_millis(300), "kept").await);
    let id = sessions.create(None).await.unwrap();

    let (_replay, rx) = sessions.start_or_attach(id, "does this survive?");
    // The turn must still be running when the last watcher goes, or this test would pass against
    // the old connection-owned behaviour too.
    assert!(
        sessions.turn_running(id),
        "precondition: the turn must be in flight when the watcher leaves"
    );
    drop(rx);
    assert!(
        sessions.turn_running(id),
        "dropping the last watcher must not end the turn"
    );

    for _ in 0..400 {
        if !sessions.turn_running(id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let history = sessions.history(id).await.unwrap();
    assert!(
        history.iter().any(|m| m.content.contains("kept")),
        "the reply must be persisted even though nobody was watching: {history:?}"
    );
}

/// A reconnect joins the running turn instead of starting a second one.
///
/// Without this, a client that resends after losing its connection pays for the same answer twice
/// and the conversation grows two copies of the question.
#[tokio::test]
async fn attaching_twice_runs_one_turn() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(sessions_at(dir.path(), vec![CompletionResponse::text("once")]).await);
    let id = sessions.create(None).await.unwrap();

    let (_r1, _rx1) = sessions.start_or_attach(id, "only once");
    let (_r2, _rx2) = sessions.start_or_attach(id, "only once");

    for _ in 0..200 {
        if !sessions.turn_running(id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let history = sessions.history(id).await.unwrap();
    let users = history.iter().filter(|m| m.role == Role::User).count();
    assert_eq!(
        users, 1,
        "a second attach started a second turn: {history:?}"
    );
}

/// Attaching mid-turn replays what already happened.
///
/// A reconnect that only showed *future* events would leave the client staring at a blank pane while
/// the answer it already missed sits in the buffer.
#[tokio::test]
async fn a_late_attach_replays_what_it_missed() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(sessions_at(dir.path(), vec![CompletionResponse::text("hello")]).await);
    let id = sessions.create(None).await.unwrap();

    let (_replay, _rx) = sessions.start_or_attach(id, "hi");
    for _ in 0..200 {
        if !sessions.turn_running(id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // The turn has retired, so there is nothing to attach to — the honest answer, not an empty feed.
    assert!(
        sessions.attach(id).is_none(),
        "a finished turn must not be attachable; it has nothing left to stream"
    );
}

/// Cancelling is now an explicit act, and it keeps the old rollback guarantee: nothing persists.
#[tokio::test]
async fn cancelling_a_turn_persists_nothing_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(sessions_at(dir.path(), vec![CompletionResponse::text("nope")]).await);
    let id = sessions.create(None).await.unwrap();

    assert!(
        !sessions.cancel_turn(id),
        "cancelling with nothing running must report that, not claim success"
    );

    let (_replay, _rx) = sessions.start_or_attach(id, "stop me");
    let cancelled = sessions.cancel_turn(id);
    assert!(cancelled, "an in-flight turn must be cancellable");
    assert!(!sessions.turn_running(id), "cancel must retire the entry");
}

/// A daemon restart mid-turn must leave a *visible* dead turn, not silence.
///
/// The restart is simulated the way the store's other durability tests do it: run a turn against a
/// provider that never answers, then drop the whole `ChatSessions` — the process dying takes the
/// in-memory registry with it — and reopen the store at the same root.
///
/// What a reader must then see: the human's message is there (persisted before inference, on
/// purpose), no reply, nothing running, and `last_turn_unanswered` saying so. A conversation that
/// ends on a question with no explanation is indistinguishable from a model that returned nothing.
#[tokio::test]
async fn a_restart_mid_turn_leaves_a_visible_unanswered_turn() {
    let dir = tempfile::tempdir().unwrap();

    let id = {
        let store = Arc::new(SessionStore::open(dir.path()).await);
        let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
        let sessions = Arc::new(ChatSessions::new(store, executor, Arc::new(NoTools)));
        let id = sessions.create(None).await.unwrap();

        let (_replay, _rx) = sessions.start_or_attach(id, "will the daemon outlive this?");
        // Let the user node land before the "process" dies.
        for _ in 0..200 {
            if sessions.history(id).await.map(|h| h.len()).unwrap_or(0) >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            sessions.turn_running(id),
            "the turn should still be in flight"
        );
        assert!(
            !sessions.last_turn_unanswered(id).await,
            "a turn that is still running must never be reported as unanswered"
        );
        id
    }; // ChatSessions dropped — the registry is gone, exactly as a restart loses it.

    // Reopen at the same root: a fresh daemon reading the durable log.
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
    let reopened = Arc::new(ChatSessions::new(store, executor, Arc::new(NoTools)));

    let history = reopened.history(id).await.unwrap();
    assert!(
        history.iter().any(|m| m.role == Role::User),
        "the question must survive the restart: {history:?}"
    );
    assert!(
        !history.iter().any(|m| m.role == Role::Assistant),
        "no reply was produced, so none may be persisted: {history:?}"
    );
    assert!(
        !reopened.turn_running(id),
        "nothing is running after a restart — reporting otherwise is the hang this guards"
    );
    assert!(
        reopened.last_turn_unanswered(id).await,
        "the dead turn must be visible, not silent"
    );
}

/// The positive control. A conversation whose turn completed is not an unanswered one — without
/// this, a function that always returned `true` would pass the test above.
#[tokio::test]
async fn a_completed_turn_is_not_reported_unanswered() {
    let dir = tempfile::tempdir().unwrap();
    let sessions =
        Arc::new(sessions_at(dir.path(), vec![CompletionResponse::text("answered")]).await);
    let id = sessions.create(None).await.unwrap();

    sessions.turn(id, "did you answer?").await.unwrap();

    assert!(!sessions.turn_running(id));
    assert!(
        !sessions.last_turn_unanswered(id).await,
        "a turn with a reply under it is answered"
    );
}
