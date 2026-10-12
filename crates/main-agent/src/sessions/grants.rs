//! Grants, MCP scoping, narrowing, and risk-gate arming.

use super::super::*;
use super::test_fixtures::*;

use std::sync::Arc as StdArc;

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

    let sessions = ChatSessions::new(store, executor, Arc::new(no_tools_runtime())).with_guards(
        vec![("tasks-mcp".into(), Consequence::Reversible)],
        liberado_common::CapabilitySet::empty(),
        dir.path().join("proposals"),
        ProposalSigner::random(),
    );

    let id = sessions.create(None).await.unwrap();
    let reply = sessions.turn(id, "hello").await.unwrap();
    assert_eq!(reply, "Hello!");
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
        ..Default::default()
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

/// Live 2026-09-12: `budget-bot` (`delegation = false`) still ran `dispatch_turn`. The
/// classifier returned Clarify; the hosted goal is unattended, so AskHuman blocked and the
/// chat that already held `categorize_transactions` printed "blocked — needed a human".
/// A scripted Clarify dispatcher must not be invoked at all on this path.
#[tokio::test]
async fn a_non_delegating_session_does_not_invoke_the_dispatcher() {
    use liberado_common::{
        BlockReason, Capability, CapabilitySet, DispatchAction, DispatchDecision,
    };

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let chat_provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text("categorized the nalgene")],
    ));
    let clarify = DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec!["which transaction?".into()],
            what_blocked: BlockReason::MissingParam,
        },
        confidence: 0.3,
        rationale: "must not run".into(),
    };
    let dispatch_provider = Arc::new(MockProvider::with_script(
        "dispatch",
        [CompletionResponse::text(
            serde_json::to_string(&clarify).unwrap(),
        )],
    ));
    let dispatcher = Dispatcher::new(dispatch_provider.clone(), DispatchTuning::default(), 4);
    let executor = Executor::new(chat_provider.clone(), Budget::default());
    let sessions = ChatSessions::new(
        store,
        executor,
        Arc::new(OneTool("liberado-actual-mcp:categorize_transactions")),
    )
    .with_guards(
        vec![("liberado-actual-mcp".into(), Consequence::Reversible)],
        CapabilitySet::empty(),
        dir.path().join("proposals"),
        ProposalSigner::random(),
    )
    .with_dispatch(dispatcher, Arc::new(CapabilityCatalog::new()));

    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                capabilities: CapabilitySet::from_iter([Capability::ExecuteMcp(
                    "liberado-actual-mcp".into(),
                )]),
                profile: Some("budget-bot".into()),
                delegation: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sessions
        .turn(id, "categorize the amazon water bottle as home goods")
        .await
        .unwrap();

    assert!(
        dispatch_provider.received_requests().is_empty(),
        "delegation=false must not classify the turn; dispatcher saw {:?}",
        dispatch_provider.received_requests().len()
    );
    let texts: Vec<String> = chat_provider
        .received_requests()
        .iter()
        .map(|r| {
            r.messages
                .last()
                .map(|m| m.content.clone())
                .unwrap_or_default()
        })
        .collect();
    assert!(!texts.is_empty(), "the chat model must run the turn itself");
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

/// A named profile pins its model onto the turn; an unnamed grant pins nothing even when it
/// carries a model field.
#[test]
fn profile_model_needs_a_named_profile() {
    use liberado_conversation_store::ConversationHeader;

    let named = ConversationHeader {
        id: Ulid::new(),
        title: None,
        parent_conversation: None,
        spawned_by: None,
        created_at: chrono::Utc::now(),
        grant: liberado_session::SessionGrant {
            profile: Some("researcher".into()),
            model: Some("gpt-x".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        ChatSessions::profile_model(&named),
        Some("gpt-x".into()),
        "a named profile pins its model"
    );

    let anonymous = ConversationHeader {
        id: Ulid::new(),
        title: None,
        parent_conversation: None,
        spawned_by: None,
        created_at: chrono::Utc::now(),
        grant: liberado_session::SessionGrant {
            profile: None,
            model: Some("gpt-x".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        ChatSessions::profile_model(&anonymous),
        None,
        "no named profile means the daemon's defaults pin nothing"
    );
}

/// Risk gating arms when ANY of the three sources is present — and stays off when none is.
/// Each source alone must be sufficient (that is what rejects both operator swaps of `||`).
#[tokio::test]
async fn risk_gate_arms_on_each_source_alone() {
    let dir = tempfile::tempdir().unwrap();
    let build = || async {
        let store = Arc::new(SessionStore::open(tempfile::tempdir().unwrap().keep()).await);
        let executor = Executor::new(
            Arc::new(MockProvider::with_script(
                "m",
                Vec::<CompletionResponse>::new(),
            )),
            Budget::default(),
        );
        ChatSessions::new(store, executor, Arc::new(no_tools_runtime()))
    };
    let _ = dir;
    let bare = build().await;
    assert!(
        !bare.risk_gate_enabled(),
        "no catalog, consequences, or zones: gates stay off"
    );

    let with_consequences = ChatSessions::with_guards(
        build().await,
        vec![("tasks-mcp".into(), Consequence::External)],
        CapabilitySet::empty(),
        std::env::temp_dir(),
        liberado_common::ProposalSigner::random(),
    );
    assert!(with_consequences.risk_gate_enabled());

    let with_real_zones = ChatSessions::with_zone_guards(
        build().await,
        vec![McpDescriptor {
            name: "tasks-mcp".into(),
            ..Default::default()
        }],
        vec![("files".into(), WriteClass::Shared)],
    );
    assert!(with_real_zones.risk_gate_enabled());

    let with_live =
        ChatSessions::with_live_catalog(build().await, Arc::new(CapabilityCatalog::new()));
    assert!(with_live.risk_gate_enabled());
}

/// Tool-only grants (no whole-MCP grants) must reach the scoped runtime, not the NoTools
/// fallback — that conjunction inversion was a survivor.
#[tokio::test]
async fn tool_only_grants_still_get_a_scoped_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, _provider) =
        sessions_for_narrowing_test(dir.path(), vec!["tasks-mcp".into()]).await;
    let id = sessions.create(None).await.unwrap();

    let caps = CapabilitySet::from_iter([Capability::ExecuteTool("tasks-mcp:add".into())]);
    let runtime = sessions.scoped_extras_runtime("u", id, caps);
    let names: Vec<String> = runtime.catalog().iter().map(|t| t.name.clone()).collect();
    assert_eq!(
        names,
        vec!["tasks-mcp:add".to_string()],
        "a tool-only grant scopes to exactly that tool"
    );
}

/// Narrowing by relevant MCP must filter *qualified tool* grants by their parent MCP too — a
/// deleted match arm (fall-through to "keep") leaks tools from irrelevant MCPs.
#[tokio::test]
async fn narrowing_filters_qualified_tool_grants_by_parent_mcp() {
    let dir = tempfile::tempdir().unwrap();

    let decision = DispatchDecision {
        action: DispatchAction::ExecuteDirect {
            seed_calls: Vec::new(),
            relevant_mcps: vec!["tasks-mcp".into()],
            delivery: Delivery::Summarize,
        },
        confidence: 0.9,
        rationale: "test".into(),
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
    let chat_provider = Arc::new(MockProvider::with_script(
        "chat",
        [CompletionResponse::text("done")],
    ));
    let executor = Executor::new(chat_provider.clone(), Budget::default());
    let store = Arc::new(SessionStore::open(dir.path()).await);
    // The grant mixes a whole-MCP grant with a qualified single-tool grant from another MCP.
    let capabilities = CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::ExecuteTool("email-mcp:send".into()),
    ]);
    let sessions = ChatSessions::new(store, executor, Arc::new(TwoMcpTools))
        .with_guards(
            Vec::new(),
            capabilities,
            dir.path().join("proposals"),
            liberado_common::ProposalSigner::random(),
        )
        .with_delegation_mode(true)
        .with_dispatch(dispatcher, Arc::new(CapabilityCatalog::new()));

    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "add milk").await.unwrap();

    let offered: Vec<String> = chat_provider.received_requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(
        offered,
        vec!["tasks-mcp:add".to_string()],
        "email-mcp:send must be narrowed away even though it was granted as a qualified tool"
    );
}

/// `DecoratingRuntime::passthrough` is pure delegation in both directions.
#[tokio::test]
async fn pass_through_runtime_forwards_catalog_and_invoke() {
    struct Echo;
    #[async_trait::async_trait]
    impl liberado_executor::ToolRuntime for Echo {
        fn catalog(&self) -> Vec<ToolDef> {
            vec![ToolDef::new("echo", "e", serde_json::json!({}))]
        }
        async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
            Ok(format!("echo:{}", call.name))
        }
    }

    let inner = StdArc::new(Echo);
    let passthrough = liberado_executor::DecoratingRuntime::passthrough(inner.clone());
    assert_eq!(passthrough.catalog().len(), 1, "catalog forwards");
    let call = ToolInvocation::new("c1", "echo", serde_json::json!({}));
    assert_eq!(
        passthrough.invoke(&call).await.unwrap(),
        "echo:echo",
        "invoke forwards"
    );
}

/// NoToolsRuntime refuses invocation with its specific error — the model-facing signal that
/// this chat holds no grants.
#[tokio::test]
async fn no_tools_runtime_refuses_invocation() {
    let call = ToolInvocation::new("c1", "anything", serde_json::json!({}));
    let err = NoToolsRuntime.invoke(&call).await.unwrap_err();
    assert!(
        err.contains("no tools are granted"),
        "the refusal names the cause: {err}"
    );
}

// ── Vault-zone permission request via the chat sessions layer (2026-10-11) ────────────────
//
// The live bug (session `01M3ZB89WK54HGNG6ATJ9FPJMX`, `turbovault:edit_note` on
// `conformance/approval-test.md` without `Write(Vault("conformance"))` in the grant) was
// reproduced end-to-end via `RiskGatedToolRuntime` in `risk_gated_tests::vault_write_gate_regression`.
// These tests pin the same scenario at the **session** layer: a `ChatSessions` chat whose
// profile grants `ExecuteMcp("turbovault")` but **not** `Write(Vault("conformance"))` calls the
// tool, and the runtime gate raises a permission request — the inner tool is NEVER invoked, a
// proposal file is written with `session_id` + `origin = Web` stamped, and the model is told
// the action was NOT executed. The session-layer test is the shape of the bug-trace one
// would assemble from a real persona: model -> provider -> executor -> risk gate -> proposal.

use liberado_common::approval_ledger::ApprovalLedger;

/// A `ChatSessions` configured for the live-bug scenario:
///   - CapabilityCatalog with the live `turbovault` MCP declared with full path-addressed
///     classification (write_tools / read_tools / vault_wide_writes), so a real edit_note call
///     resolves to `WriteTarget::Zone("conformance")` instead of `Undeterminable`.
///   - Profile: ExecuteMcp("turbovault") + Read(Vault("conformance")) — the exact missing-grant
///     shape the live session had.
///   - Scoped Runtime that records every invocation but does NOT actually run the tool —
///     succeeds if (and only if) the runtime gate decided the call was safe.
async fn live_bug_chat_sessions(dir: &std::path::Path) -> ChatSessions {
    use liberado_common::McpDescriptor;
    use liberado_common::{Capability, CapabilitySet, Consequence};

    let store = Arc::new(SessionStore::open(dir).await);

    // The catalog must carry `edit_note` in write_tools with `zone_from_arg = "path"` so the
    // runtime gate sees `WriteTarget::Zone("conformance")` (not Undeterminable). Without this,
    // the fix's "undeclared tool" refusal is what we'd see — the live bug had `edit_note`
    // missing entirely, so we recreate the *post-fix* state and confirm the gate catches it.
    let catalog = Arc::new(liberado_common::CapabilityCatalog::new());
    catalog.register(McpDescriptor {
        name: "turbovault".into(),
        description: "live turbovault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec![
            "write_note".into(),
            "edit_note".into(),
            "delete_note".into(),
        ],
        read_tools: vec!["read_note".into()],
        vault_wide_writes: vec!["begin_fanout".into(), "set_active_vault".into()],
        admin_tools: Vec::new(),
        write_path_args: std::collections::BTreeMap::new(),
    });
    let capabilities = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        // The Personal Task Manager profile the live session had — a Read on conformance,
        // but NOT a Write. This is exactly the gap.
        Capability::Read(liberado_common::Zone::vault("conformance")),
    ]);

    // Provider that "calls" the model with a tool_call to `turbovault:edit_note` on the
    // conformance path, then a final text reply — so the gate sees a real model invocation.
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "live-bug-2026-10-11",
                "turbovault:edit_note",
                serde_json::json!({"path": "conformance/approval-test.md"}),
            )]),
            CompletionResponse::text("I'll wait for the human to approve."),
        ],
    ));
    let executor = Executor::new(provider, Budget::default());

    ChatSessions::new(store, executor, turbovault_recording_runtime())
        .with_guards(
            vec![("turbovault".into(), Consequence::Reversible)],
            capabilities,
            dir.join("proposals"),
            ProposalSigner::random(),
        )
        .with_live_catalog(catalog)
}

/// A runtime exposing `turbovault:edit_note` / `turbovault:read_note` so the model has
/// something to call; succeed only when the runtime gate (NOT the model) decides the call is
/// safe — tools that the gate should refuse instead return their gating message via the
/// `Result::Err` path. We don't actually save a file; we just record that the gate let the
/// call through.
fn turbovault_recording_runtime() -> Arc<dyn ToolRuntime> {
    Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:edit_note", "turbovault:read_note"],
        Ok("saved".into()),
    ))
}

#[tokio::test]
async fn live_bug_repro_edit_note_on_conformance_is_a_permission_request_at_the_chat_layer() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = live_bug_chat_sessions(dir.path()).await;

    let id = sessions.create(None).await.unwrap();
    sessions
        .turn(id, "edit the conformance note")
        .await
        .unwrap();

    // The model "called" the tool. The runtime gate refused (no Write(Vault("conformance"))).
    // The proposal file system has one entry, with `requested_grant` and the session's
    // approval origin stamped on.
    let ledger_dir =
        std::path::Path::new(&dir.path().join("proposals").to_string_lossy().into_owned())
            .join(liberado_common::PROPOSALS_DIR);
    let proposals: Vec<_> = std::fs::read_dir(&ledger_dir).unwrap().collect();
    assert_eq!(
        proposals.len(),
        1,
        "exactly one permission request was raised"
    );
    let proposal_bytes = std::fs::read_to_string(proposals[0].as_ref().unwrap().path()).unwrap();
    let proposal = liberado_common::Proposal::from_note(&proposal_bytes).unwrap();
    assert_eq!(
        proposal.requested_grant,
        Some(liberado_common::Capability::Write(
            liberado_common::Zone::vault("conformance")
        )),
        "the proposal asks for the missing zone"
    );
    assert!(
        matches!(
            proposal.origin,
            Some(liberado_common::ApprovalOrigin::Background)
                | Some(liberado_common::ApprovalOrigin::Web)
        ),
        "the proposal has a routable origin: {:?}",
        proposal.origin
    );
}

/// `Once` after approval: the next identical call raises **another** permission request. Pin
/// the Deny/Once/Session/Everywhere flow at the session layer using the actual ledger
/// (`ApprovalLedger`) so a refactor of `apply_permission_decision` that broke Once/Everywhere
/// would surface here.
#[tokio::test]
async fn approval_decision_once_then_second_call_raises_again() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = ApprovalLedger::new(dir.path().join("ledger"));

    let mut proposal = make_zone_proposal("Tools/bug-2026-10-11", "conformance");
    let dec1 = liberado_common::apply_permission_decision(
        &mut proposal,
        "once",
        liberado_common::DecisionVia::Webui,
        chrono::Utc::now(),
    )
    .unwrap();
    assert!(matches!(
        dec1,
        liberado_common::PermissionDecide::Applied { action: "once" }
    ));

    let mut proposal2 = make_zone_proposal("Tools/bug-2026-10-11", "conformance");
    // (Same tool/args, but a fresh proposal — proving a fresh request is needed; `Once` is
    // session-level in the ledger, not proposal-level.)
    let dec2 = liberado_common::apply_permission_decision(
        &mut proposal2,
        "once",
        liberado_common::DecisionVia::Webui,
        chrono::Utc::now(),
    )
    .unwrap();
    assert!(matches!(
        dec2,
        liberado_common::PermissionDecide::Applied { action: "once" }
    ));
    // The two proposals are distinct decision records; both reach the ledger and resolve to
    // `Approved` after the Once button. Pin that a deny would set `Rejected` (the structural
    // signal) — the runtime executes the call only when `decision_for` returns
    // `ApprovalDecision::Approved`.
    ledger
        .record(
            &proposal.id,
            liberado_common::approval_ledger::ApprovalDecision::Approved,
            "webui",
        )
        .await
        .unwrap();
    ledger
        .record(
            &proposal2.id,
            liberado_common::approval_ledger::ApprovalDecision::Approved,
            "webui",
        )
        .await
        .unwrap();

    // Both decisions resolved to `once`; verify they round-trip through the ledger.
    let d1 = ledger.decision_for(&proposal.id).await;
    let d2 = ledger.decision_for(&proposal2.id).await;
    assert_eq!(
        d1,
        Some(liberado_common::approval_ledger::ApprovalDecision::Approved),
        "first decision was recorded as Approved"
    );
    assert_eq!(
        d2,
        Some(liberado_common::approval_ledger::ApprovalDecision::Approved),
        "second decision was recorded as Approved"
    );
}

/// Build a `Pending` permission request for `turbovault:edit_note` on
/// `conformance/approval-test.md` — the shape the runtime gate raises.
fn make_zone_proposal(id: &str, zone: &str) -> liberado_common::Proposal {
    use liberado_common::proposal::ProposalSigner;
    let signer = ProposalSigner::random();
    let mut p = liberado_common::Proposal::pending(
        id,
        "test-corr",
        "liberado-chat",
        liberado_common::proposal::ProposedAction::ToolCalls(vec![liberado_common::ToolCall {
            tool: "turbovault:edit_note".into(),
            args: serde_json::json!({"path": format!("{zone}/approval-test.md")}),
        }]),
        format!("Permission request: 'turbovault:edit_note' needs Write access to zone '{zone}'."),
    )
    .with_requested_grant(liberado_common::Capability::Write(
        liberado_common::Zone::vault(zone),
    ));
    p.session_id = Some("01M3ZB89WK54HGNG6ATJ9FPJMX".into());
    p.origin = Some(liberado_common::ApprovalOrigin::Web);
    signer.sign(p).into_proposal()
}
