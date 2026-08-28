//! Shared builders and stub runtimes for [`ChatSessions`] tests.
//!
//! These are the fixtures the suite already used; this module does not add types.

use super::super::*;
pub(crate) use async_trait::async_trait;
pub(crate) use liberado_common::{BlockReason, Delivery, DispatchDecision};
pub(crate) use liberado_config_loader::DispatchTuning;
pub(crate) use liberado_executor::{Budget, RuntimeFactory, RuntimeSetupError};
pub(crate) use liberado_provider::{
    AgentRole, CompletionRequest, CompletionResponse, LatencyEvent, LatencyRecorder,
    MeteredProvider, MockProvider, Provider, ProviderError, ProviderResult, ToolDef,
    ToolInvocation,
};
pub(crate) use liberado_session_store::SessionStore;
pub(crate) use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

pub(crate) struct NoTools;
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
pub(crate) struct PendingProvider;
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
pub(crate) struct SlowProvider {
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
pub(crate) async fn slow_sessions_at(
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
pub(crate) async fn sessions_at(
    root: &std::path::Path,
    replies: Vec<CompletionResponse>,
) -> ChatSessions {
    let store = Arc::new(SessionStore::open(root).await);
    let provider = Arc::new(MockProvider::with_script("mock", replies));
    let executor = Executor::new(provider, Budget::default());
    ChatSessions::new(store, executor, Arc::new(NoTools))
}

/// A runtime that always offers one tool, so we can assert what the model is shown.
pub(crate) struct OneTool(pub(crate) &'static str);
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

pub(crate) struct NoopFactory;
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
pub(crate) async fn sessions_with_dispatch(
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

/// A runtime offering tools from two different MCP namespaces, so narrowing between them is
/// observable.
pub(crate) struct TwoMcpTools;
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
pub(crate) async fn sessions_for_narrowing_test(
    dir: &std::path::Path,
    relevant_mcps: Vec<String>,
) -> (ChatSessions, Arc<MockProvider>) {
    use liberado_common::Capability;

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps,
            delivery: Delivery::Summarize,
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

/// A `ChatSessions` with compaction wired, over the real session store. One `MockProvider` serves
/// **both** the executor and the summarizer, so the script interleaves in call order (a
/// compaction's summary request consumes the next scripted response before the turn's reply).
pub(crate) async fn compacting_sessions_at(
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
pub(crate) async fn seed_turns(sessions: &ChatSessions, id: Ulid, pairs: &[(&str, &str)]) {
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

#[derive(Default)]
pub(crate) struct CapturingRecorder {
    pub(crate) events: Mutex<Vec<LatencyEvent>>,
}

impl LatencyRecorder for CapturingRecorder {
    fn record(&self, event: LatencyEvent) {
        self.events.lock().unwrap().push(event);
    }
}

/// `ChatSessions` whose face provider is a `MeteredProvider` over a scripted mock.
pub(crate) async fn metered_sessions_at(
    root: &std::path::Path,
    replies: Vec<CompletionResponse>,
    rec: Arc<CapturingRecorder>,
) -> ChatSessions {
    let store = Arc::new(SessionStore::open(root).await);
    let inner: Arc<dyn Provider> = Arc::new(MockProvider::with_script("mock", replies));
    let recorder: Arc<dyn LatencyRecorder> = rec;
    let provider = MeteredProvider::wrap(inner, AgentRole::Face, recorder);
    let executor = Executor::new(provider, Budget::default());
    ChatSessions::new(store, executor, Arc::new(NoTools))
}
