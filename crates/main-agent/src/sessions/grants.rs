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
        ChatSessions::new(store, executor, Arc::new(NoTools))
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

/// PassThroughRuntime is pure delegation in both directions.
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
    let passthrough = PassThroughRuntime(inner.clone());
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
