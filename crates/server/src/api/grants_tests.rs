//! Read-only conversation grants: the projector, then the HTTP route.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chat_client_contract::{ConversationGrantsResponse, GrantSource};
use http_body_util::BodyExt;
use liberado_common::{Capability, CapabilitySet, WriteClass, Zone};
use liberado_conversation_store::Ulid;
use liberado_executor::{Budget, Executor};
use liberado_main_agent::ChatSessions;
use liberado_provider::MockProvider;
use liberado_session::SessionGrant;
use liberado_session_store::SessionStore;
use liberado_test_support::NoopRuntime;
use tower::ServiceExt;

use super::project_conversation_grants;
use crate::state::AppState;

fn classes(zone: &str) -> String {
    if zone == "notes" {
        "agent_writable".into()
    } else {
        "proposal_only".into()
    }
}

fn agent_caps() -> CapabilitySet {
    CapabilitySet::from_iter([
        Capability::ExecuteMcp("tasks-mcp".into()),
        Capability::Read(Zone::vault("tasks")),
        Capability::AskHuman,
    ])
}

fn connected() -> Vec<String> {
    vec![
        "tasks-mcp:add".into(),
        "tasks-mcp:list".into(),
        "email-mcp:send".into(),
        "delegate".into(),
    ]
}

#[test]
fn an_unnamed_header_uses_the_agent_grant_and_ignores_stored_caps() {
    let leftover = CapabilitySet::from_iter([Capability::ExecuteTool("email-mcp:send".into())]);
    let view = project_conversation_grants(
        "01CONV",
        None,
        &agent_caps(),
        &leftover,
        &connected(),
        classes,
    );
    assert_eq!(view.effective, GrantSource::Agent);
    assert!(view.profile.is_none());
    assert!(view.session.is_empty());
    assert_eq!(view.agent.mcps, vec!["tasks-mcp".to_string()]);
    assert!(view.agent.tools.is_empty());
    assert!(view.agent.ask_human);
    assert_eq!(view.agent.zones[0].name, "tasks");
    assert_eq!(view.agent.zones[0].access, "read");
    assert_eq!(view.agent.zones[0].write_class, "proposal_only");
    assert_eq!(
        view.callable,
        vec!["tasks-mcp:add".to_string(), "tasks-mcp:list".to_string()]
    );
}

#[test]
fn a_named_empty_profile_does_not_fall_back_to_the_agent() {
    let view = project_conversation_grants(
        "01CONV",
        Some("quiet"),
        &agent_caps(),
        &CapabilitySet::empty(),
        &connected(),
        classes,
    );
    assert_eq!(view.effective, GrantSource::Session);
    assert_eq!(view.profile.as_deref(), Some("quiet"));
    assert!(view.session.is_empty());
    assert!(view.callable.is_empty());
    assert_eq!(view.agent.mcps, vec!["tasks-mcp".to_string()]);
}

#[test]
fn a_named_profile_lists_its_tools_zones_and_only_its_callable_set() {
    let session = CapabilitySet::from_iter([
        Capability::ExecuteTool("turbovault:read_note".into()),
        Capability::Write(Zone::vault("notes")),
    ]);
    let view = project_conversation_grants(
        "01CONV",
        Some("research"),
        &agent_caps(),
        &session,
        &connected(),
        classes,
    );
    assert_eq!(view.effective, GrantSource::Session);
    assert_eq!(view.session.tools, vec!["turbovault:read_note".to_string()]);
    assert!(view.session.mcps.is_empty());
    assert!(!view.session.ask_human);
    assert_eq!(view.session.zones[0].name, "notes");
    assert_eq!(view.session.zones[0].access, "write");
    assert_eq!(view.session.zones[0].write_class, "agent_writable");
    assert!(view.callable.is_empty());
}

struct Harness {
    app: Router,
    chat: Arc<ChatSessions>,
    state: Arc<AppState>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let chat = Arc::new(ChatSessions::new(
        sessions.clone(),
        Executor::new(
            Arc::new(MockProvider::with_script("mock", Vec::new())),
            Budget::default(),
        ),
        Arc::new(NoopRuntime),
    ));
    let mut state = AppState::for_test(sessions, Some(chat.clone()), dir.path().to_path_buf());
    state.main_agent_capabilities = agent_caps();
    state.chat_tool_names = connected();
    let mut config = (*state.config).clone();
    config.policy.zones.push(liberado_config::ZonePolicy {
        zone: "notes".into(),
        write_class: WriteClass::AgentWritable,
    });
    state.config = Arc::new(config);
    let state = Arc::new(state);
    // The production router, so a second method on this path fails here rather than
    // only on a router the test built with GET alone.
    let app = crate::build_app_router(&state);
    Harness {
        app,
        chat,
        state,
        _dir: dir,
    }
}

async fn call(app: &Router, method: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn get_grants(app: &Router, id: Ulid) -> ConversationGrantsResponse {
    let (status, body) = call(app, "GET", &format!("/api/conversations/{id}/grants")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[tokio::test]
async fn the_route_reports_agent_and_session_grants_without_writing() {
    let h = harness().await;
    let plain = h.chat.create(Some("plain".into())).await.unwrap();
    let before = h.state.sessions.session(plain).await.unwrap();

    let view = get_grants(&h.app, plain).await;
    assert_eq!(view.id, plain.to_string());
    assert_eq!(view.effective, GrantSource::Agent);
    assert_eq!(view.agent_component, "main-agent");
    assert!(view.session.is_empty());
    assert_eq!(
        view.callable,
        vec!["tasks-mcp:add".to_string(), "tasks-mcp:list".to_string()]
    );
    let after = h.state.sessions.session(plain).await.unwrap();
    assert_eq!(after.grant, before.grant);

    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/api/conversations/{plain}/grants"),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);

    let grant = SessionGrant {
        profile: Some("research".into()),
        capabilities: CapabilitySet::from_iter([
            Capability::ExecuteTool("turbovault:read_note".into()),
            Capability::Write(Zone::vault("notes")),
        ]),
        ..SessionGrant::default()
    };
    h.chat.set_profile(plain, grant).await.unwrap();
    let view = get_grants(&h.app, plain).await;
    assert_eq!(view.effective, GrantSource::Session);
    assert_eq!(view.profile.as_deref(), Some("research"));
    assert_eq!(view.session.tools, vec!["turbovault:read_note".to_string()]);
    assert_eq!(view.session.zones[0].write_class, "agent_writable");
    assert!(view.callable.is_empty());
    assert_eq!(view.agent.mcps, vec!["tasks-mcp".to_string()]);
}

#[tokio::test]
async fn unknown_disabled_and_private_conversations() {
    let h = harness().await;
    let missing = Ulid::new();
    let (status, body) = call(
        &h.app,
        "GET",
        &format!("/api/conversations/{missing}/grants"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("conversation not found"), "{body}");

    let (status, _) = call(&h.app, "GET", "/api/conversations/not-a-ulid/grants").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let ghost = h
        .chat
        .create_incognito(Some("private".into()))
        .await
        .unwrap();
    let view = get_grants(&h.app, ghost).await;
    assert_eq!(view.effective, GrantSource::Agent);

    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let id = sessions
        .create_session(liberado_session_store::NewSession::default())
        .await
        .id;
    let state = Arc::new(AppState::for_test(sessions, None, dir.path().to_path_buf()));
    let app = crate::build_app_router(&state);
    let (status, body) = call(&app, "GET", &format!("/api/conversations/{id}/grants")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("chat is disabled"), "{body}");
}
