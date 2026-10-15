//! A profile's model goes to the provider the profile names.

use std::collections::HashMap;
use std::sync::Arc;

use super::super::*;
use super::test_fixtures::*;

const PINNED: &str = "deepseek/deepseek-v4-flash";

fn dogfood_grant() -> SessionGrant {
    SessionGrant {
        profile: Some("dogfood".into()),
        model: Some(PINNED.into()),
        ..SessionGrant::default()
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    sessions: ChatSessions,
    default_provider: Arc<MockProvider>,
    openrouter: Arc<MockProvider>,
}

async fn harness(replies_each: usize) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let default_provider = Arc::new(MockProvider::with_script(
        "minimax-default",
        std::iter::repeat_with(|| CompletionResponse::text("from-default")).take(replies_each),
    ));
    let openrouter = Arc::new(MockProvider::with_script(
        "openrouter-native",
        std::iter::repeat_with(|| CompletionResponse::text("from-openrouter")).take(replies_each),
    ));
    let executor = Executor::new(default_provider.clone(), Budget::default());
    let mut backends: HashMap<String, Arc<dyn Provider>> = HashMap::new();
    backends.insert("openrouter".into(), openrouter.clone());
    let sessions = ChatSessions::new(store, executor, Arc::new(no_tools_runtime()))
        .with_profile_providers(HashMap::new(), backends, HashMap::new());
    Harness {
        _dir: dir,
        sessions,
        default_provider,
        openrouter,
    }
}

fn sent_model(provider: &MockProvider) -> Option<String> {
    provider.last_request().and_then(|request| request.model)
}

#[tokio::test]
async fn a_profile_with_provider_routes_its_model_there() {
    let harness = harness(1).await;
    harness
        .sessions
        .set_profile_route("dogfood", Some("openrouter"));
    let id = harness
        .sessions
        .create_with_grant(None, dogfood_grant())
        .await
        .unwrap();

    harness.sessions.turn(id, "hello").await.unwrap();

    assert!(
        harness.default_provider.last_request().is_none(),
        "the default provider must not see the profile's model"
    );
    assert_eq!(sent_model(&harness.openrouter).as_deref(), Some(PINNED));
}

#[tokio::test]
async fn a_profile_without_provider_uses_the_default() {
    let harness = harness(1).await;
    let id = harness
        .sessions
        .create_with_grant(None, dogfood_grant())
        .await
        .unwrap();

    harness.sessions.turn(id, "hello").await.unwrap();

    assert!(harness.openrouter.last_request().is_none());
    assert_eq!(
        sent_model(&harness.default_provider).as_deref(),
        Some(PINNED)
    );
}

#[tokio::test]
async fn a_pinned_model_session_reresolves_the_provider_after_the_profile_changes() {
    let harness = harness(2).await;
    let id = harness
        .sessions
        .create_with_grant(None, dogfood_grant())
        .await
        .unwrap();

    harness.sessions.turn(id, "hello").await.unwrap();
    assert_eq!(harness.default_provider.received_requests().len(), 1);
    assert_eq!(
        sent_model(&harness.default_provider).as_deref(),
        Some(PINNED),
        "the first turn pins the slug on the default provider"
    );

    harness
        .sessions
        .set_profile_route("dogfood", Some("openrouter"));
    harness.sessions.turn(id, "again").await.unwrap();

    assert_eq!(
        harness.default_provider.received_requests().len(),
        1,
        "the pinned session must leave the default provider after the profile change"
    );
    assert_eq!(sent_model(&harness.openrouter).as_deref(), Some(PINNED));
}

#[tokio::test]
async fn an_unavailable_provider_fails_the_turn_instead_of_using_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let default_provider = Arc::new(MockProvider::with_script(
        "minimax-default",
        [CompletionResponse::text("should-not-run")],
    ));
    let executor = Executor::new(default_provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(no_tools_runtime()))
        .with_profile_providers(
            HashMap::from([("dogfood".into(), "openrouter".into())]),
            HashMap::new(),
            HashMap::from([(
                "openrouter".into(),
                "no credentials: environment variable 'OPENROUTER_API_KEY' is unset".into(),
            )]),
        );
    let id = sessions
        .create_with_grant(None, dogfood_grant())
        .await
        .unwrap();

    let err = sessions.turn(id, "hello").await.expect_err("missing key");
    let msg = err.to_string();
    assert!(msg.contains("OPENROUTER_API_KEY"), "{msg}");
    assert!(
        default_provider.last_request().is_none(),
        "a missing key must not fall through to the default provider"
    );
}
