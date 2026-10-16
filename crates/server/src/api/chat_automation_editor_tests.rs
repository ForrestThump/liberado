//! Singleton Automation Editor over `POST /api/conversations`.

use std::str::FromStr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use liberado_executor::{Budget, Executor};
use liberado_main_agent::ChatSessions;
use liberado_provider::MockProvider;
use liberado_session::SessionGrant;
use liberado_session_store::SessionStore;
use liberado_test_support::NoopRuntime;
use tower::ServiceExt;

use super::create_conversation;

fn router(state: Arc<crate::state::AppState>) -> Router {
    Router::new()
        .route(
            "/api/conversations",
            axum::routing::post(create_conversation),
        )
        .route(
            "/api/conversations/{id}",
            axum::routing::delete(crate::api::delete_conversation),
        )
        .with_state(state)
}

async fn post(app: &Router, body: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/conversations")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn delete(app: &Router, uri: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn automation_editor_post_is_one_row_and_delete_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let chat = Arc::new(
        ChatSessions::new(
            sessions.clone(),
            Executor::new(
                Arc::new(MockProvider::with_script("mock", vec![])),
                Budget::default(),
            ),
            Arc::new(NoopRuntime),
        )
        .with_profile_resolver(Arc::new(|name: &str| {
            Ok(SessionGrant {
                profile: Some(name.to_owned()),
                delegation: Some(false),
                ..SessionGrant::default()
            })
        })),
    );
    let state = Arc::new(crate::state::AppState::for_test(
        sessions.clone(),
        Some(chat.clone()),
        dir.path().to_path_buf(),
    ));
    let app = router(state);
    let body = r#"{"automation_editor":true,"profile":"coding","title":"ignored"}"#;
    let (status, raw) = post(&app, body).await;
    assert_eq!(status, StatusCode::CREATED, "{raw}");
    let created: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["automation_editor"], true);
    assert_ne!(created["agent_creator"], true);
    assert_eq!(created["surface_mode"], "chat");
    assert_eq!(created["title"], "Automation Editor");
    assert_eq!(created["grant"]["profile"], "automation-editor");

    let (again_status, again_raw) = post(&app, body).await;
    assert_eq!(again_status, StatusCode::OK, "{again_raw}");
    let again: serde_json::Value = serde_json::from_str(&again_raw).unwrap();
    assert_eq!(again["id"], id);

    let both = r#"{"automation_editor":true,"agent_creator":true}"#;
    let (both_status, both_raw) = post(&app, both).await;
    assert_eq!(both_status, StatusCode::BAD_REQUEST, "{both_raw}");

    let ulid = liberado_conversation_store::Ulid::from_str(&id).unwrap();
    let nodes = chat.history_nodes(ulid).await.unwrap();
    let visible: Vec<_> = nodes
        .iter()
        .filter(|node| !matches!(node.author, liberado_conversation_store::Author::System))
        .collect();
    assert_eq!(visible.len(), 1);
    assert_eq!(
        visible[0].message.content,
        "Which schedule should I list or change?"
    );
    let system = nodes
        .iter()
        .find(|node| matches!(node.author, liberado_conversation_store::Author::System))
        .unwrap();
    assert!(
        system.message.content.contains("list_schedules"),
        "{system:?}"
    );
    assert!(system.message.content.contains("Approve"), "{system:?}");

    let (del_status, del_raw) = delete(&app, &format!("/api/conversations/{id}")).await;
    assert_eq!(del_status, StatusCode::CONFLICT, "{del_raw}");
    assert!(del_raw.contains("cannot be deleted"), "{del_raw}");
    assert!(sessions.session(ulid).await.unwrap().automation_editor);
}

#[tokio::test]
async fn editor_open_and_a_later_turn_resolve_without_a_topology_row() {
    let mut cfg = liberado_config::Config::default();
    cfg.topology.vault_path = std::path::PathBuf::from("/tmp/vault");
    cfg.policy.grants = vec![
        liberado_config::Grant {
            component: "main-agent".into(),
            capabilities: vec![liberado_common::Capability::AskHuman],
        },
        liberado_config::Grant {
            component: "automation-editor".into(),
            capabilities: vec![liberado_common::Capability::ExecuteTool(
                "automation-editor:list_schedules".into(),
            )],
        },
    ];
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        vec![
            liberado_provider::CompletionResponse::text("Listed."),
            liberado_provider::CompletionResponse::text("Still listed."),
        ],
    ));
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let chat = crate::chat_agent_spawn::install(
        ChatSessions::new(
            sessions.clone(),
            Executor::new(provider.clone(), Budget::default()),
            Arc::new(NoopRuntime),
        ),
        crate::surfaces::ProfileCatalog::memory(Arc::new(cfg)),
    );
    let (id, created) = chat.open_automation_editor().await.unwrap();
    assert!(created);
    let header = sessions.session(id).await.unwrap();
    assert!(header.automation_editor);
    assert!(!header.agent_creator);
    assert_eq!(header.grant.profile.as_deref(), Some("automation-editor"));
    assert_eq!(header.grant.delegation, Some(false));
    assert_eq!(
        header.grant.capabilities.capabilities,
        vec![liberado_common::Capability::ExecuteTool(
            "automation-editor:list_schedules".into()
        )]
    );
    assert!(!header.grant.capabilities.grants_ask_human());

    let first = chat.turn(id, "list schedules").await.unwrap();
    let second = chat.turn(id, "list them again").await.unwrap();
    assert_eq!(first, "Listed.");
    assert_eq!(second, "Still listed.");
    let requests = provider.received_requests();
    assert_eq!(requests.len(), 2, "both turns must reach the model");
    for request in &requests {
        let names: Vec<_> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                liberado_main_agent::LIST_SCHEDULES_TOOL_NAME,
                liberado_main_agent::CREATE_SCHEDULE_TOOL_NAME,
                liberado_main_agent::UPDATE_SCHEDULE_TOOL_NAME,
                liberado_main_agent::DELETE_SCHEDULE_TOOL_NAME,
            ]
        );
    }
    let after = sessions.session(id).await.unwrap();
    assert_eq!(after.grant.capabilities, header.grant.capabilities);
    assert!(after.automation_editor);
}
