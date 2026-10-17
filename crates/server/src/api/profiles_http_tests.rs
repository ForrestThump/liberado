use std::str::FromStr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use liberado_bootstrap::Config;
use liberado_executor::{Budget, Executor};
use liberado_main_agent::ChatSessions;
use liberado_provider::MockProvider;
use liberado_session::SessionGrant;
use liberado_session_store::SessionStore;
use liberado_test_support::NoopRuntime;
use tower::ServiceExt;

use super::{delete_profile, rename_profile};
use crate::api::list_profiles;
use crate::state::AppState;

const PROFILES: &str = r#"
[topology]
vault_path = "/tmp/vault"

[[topology.session_profiles]]
name = "coding"

[[topology.session_profiles]]
name = "life"

[[topology.session_profiles]]
name = "chat-default"

[[topology.session_profiles]]
name = "operator"

[[policy.grants]]
component = "coding"
capabilities = []

[[policy.grants]]
component = "life"
capabilities = []

[[policy.grants]]
component = "chat-default"
capabilities = []

[[policy.grants]]
component = "operator"
capabilities = []
"#;

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
        Executor::new(Arc::new(MockProvider::new("mock")), Budget::default()),
        Arc::new(NoopRuntime),
    ));
    let state = Arc::new(AppState::for_test_with_config(
        sessions,
        Some(chat.clone()),
        dir.path().to_path_buf(),
        Config::from_str(PROFILES).unwrap(),
    ));
    let app = Router::new()
        .route("/api/profiles", axum::routing::get(list_profiles))
        .route(
            "/api/profiles/{name}",
            axum::routing::patch(rename_profile).delete(delete_profile),
        )
        .with_state(state.clone());
    Harness {
        app,
        chat,
        state,
        _dir: dir,
    }
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    let request = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        builder.body(Body::from(body.to_string()))
    } else {
        builder.body(Body::empty())
    }
    .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn rename_moves_bound_conversations_and_delete_falls_back_to_default() {
    let h = harness().await;
    let id = h
        .chat
        .create_with_grant(
            Some("Budget".into()),
            SessionGrant {
                profile: Some("coding".into()),
                ..SessionGrant::default()
            },
        )
        .await
        .unwrap();
    let kept = h
        .chat
        .create_with_grant(
            Some("Notes".into()),
            SessionGrant {
                profile: Some("life".into()),
                ..SessionGrant::default()
            },
        )
        .await
        .unwrap();

    let (status, body) = call(
        &h.app,
        "PATCH",
        "/api/profiles/coding",
        Some(r#"{"name":"builder"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("builder"), "{body}");
    let header = h.state.sessions.session(id).await.unwrap();
    assert_eq!(header.grant.profile.as_deref(), Some("builder"));

    let (status, body) = call(&h.app, "GET", "/api/profiles", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("builder"), "{body}");
    assert!(!body.contains("\"coding\""), "{body}");

    let (status, body) = call(&h.app, "DELETE", "/api/profiles/builder", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"fallback\":\"default\""), "{body}");
    let header = h.state.sessions.session(id).await.unwrap();
    assert!(header.grant.profile.is_none(), "{header:?}");
    assert!(h.state.sessions.session(id).await.is_some());
    let other = h.state.sessions.session(kept).await.unwrap();
    assert_eq!(other.grant.profile.as_deref(), Some("life"));
}

#[tokio::test]
async fn built_in_profiles_are_refused_and_unknown_is_404() {
    let h = harness().await;
    for name in ["operator", "chat-default"] {
        let (status, body) = call(&h.app, "DELETE", &format!("/api/profiles/{name}"), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{name} {body}");
        let (status, body) = call(
            &h.app,
            "PATCH",
            &format!("/api/profiles/{name}"),
            Some(r#"{"name":"other"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{name} {body}");
    }
    let (status, _) = call(&h.app, "DELETE", "/api/profiles/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = call(
        &h.app,
        "PATCH",
        "/api/profiles/coding",
        Some(r#"{"name":"life"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}
