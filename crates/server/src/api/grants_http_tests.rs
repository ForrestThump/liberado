use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use liberado_common::{Capability, CapabilitySet, Zone};
use liberado_conversation_store::{ConversationStore, NewConversation, SurfaceMode};
use liberado_session::SessionGrant;
use liberado_session_store::SessionStore;
use tower::ServiceExt;

use super::get_header_grant;
use crate::state::AppState;

fn grant(profile: &str, zone: &str) -> SessionGrant {
    let mut capabilities = CapabilitySet::empty();
    capabilities.grant(Capability::Write(Zone::vault(zone)));
    capabilities.grant(Capability::ExecuteMcp("turbovault".into()));
    SessionGrant {
        capabilities,
        profile: Some(profile.to_string()),
        delegation: Some(false),
        ..SessionGrant::default()
    }
}

async fn created(store: &SessionStore, title: &str, mode: SurfaceMode, profile: &str) -> String {
    let header = store
        .create(NewConversation {
            title: Some(title.to_string()),
            grant: grant(profile, "tasks"),
            surface_mode: mode,
            ..NewConversation::default()
        })
        .await
        .unwrap();
    header.id.to_string()
}

#[tokio::test]
async fn header_grant_is_the_stored_grant_for_a_chat_and_an_agent() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let agent = created(&sessions, "Coder", SurfaceMode::Agent, "coding").await;
    let chat = created(&sessions, "Talk", SurfaceMode::Chat, "chat-default").await;
    let state = Arc::new(AppState::for_test(sessions, None, dir.path().to_path_buf()));
    let app = Router::new()
        .route(
            "/api/conversations/{id}/grant",
            axum::routing::get(get_header_grant),
        )
        .with_state(state);

    let agent_body = get_grant(&app, &agent).await;
    assert_eq!(agent_body["id"], agent);
    assert_eq!(agent_body["surface_mode"], "agent");
    assert_eq!(agent_body["grant"]["profile"], "coding");
    assert_eq!(agent_body["grant"]["delegation"], false);
    let caps = agent_body["grant"]["capabilities"]["capabilities"]
        .as_array()
        .unwrap();
    assert!(
        caps.iter().any(|cap| cap["Write"]["Vault"] == "tasks"),
        "{agent_body}"
    );

    let chat_body = get_grant(&app, &chat).await;
    assert_eq!(chat_body["surface_mode"], "chat");
    assert_eq!(chat_body["grant"]["profile"], "chat-default");

    let missing = app
        .oneshot(
            Request::builder()
                .uri("/api/conversations/01ARZ3NDEKTSV4RRFFQ69G5FAV/grant")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

async fn get_grant(app: &Router, id: &str) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/conversations/{id}/grant"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
