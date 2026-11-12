//! `POST /api/chat` must open a session the same way the stream does.
//!
//! The non-streaming handler used to call `sessions.create(None)` and drop `profile`,
//! `incognito`, and `background`. A client that asked for a named profile then ran on the
//! default grant. These tests drive both handlers and read the stored session.

use std::str::FromStr;
use std::sync::Arc;

use liberado_common::{Capability, Zone};
use liberado_conversation_store::{ConversationStore, Ulid};
use liberado_executor::{Budget, Executor};
use liberado_main_agent::ChatSessions;
use liberado_provider::MockProvider;
use liberado_session::Visibility;
use liberado_session_store::SessionStore;
use liberado_test_support::NoopRuntime;

use super::*;

const PROFILES: &str = r#"
[topology]
vault_path = "/tmp/vault"

[[topology.session_profiles]]
name = "budget-bot"
# A bare slug. The daemon default provider's default_model has no '/'.
# A vendor/model id here fails load unless the profile names a provider that accepts one.
model = "budget-small"
prompt_append = "Stay inside the budget."
read = ["finance"]

[[topology.session_profiles]]
name = "retired"
enabled = false

[[policy.zones]]
zone = "finance"
write_class = "human_only"
"#;

struct Harness {
    state: Arc<AppState>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let chat = Arc::new(ChatSessions::new(
        sessions.clone(),
        Executor::new(
            Arc::new(MockProvider::with_script(
                "mock",
                vec![
                    liberado_provider::CompletionResponse::text("ok"),
                    liberado_provider::CompletionResponse::text("ok"),
                ],
            )),
            Budget::default(),
        ),
        Arc::new(NoopRuntime),
    ));
    let state = Arc::new(AppState::for_test_with_config(
        sessions,
        Some(chat),
        dir.path().to_path_buf(),
        liberado_bootstrap::Config::from_str(PROFILES).unwrap(),
    ));
    Harness { state, _dir: dir }
}

fn request(
    session: Option<Ulid>,
    incognito: bool,
    background: bool,
    profile: Option<&str>,
) -> ChatRequest {
    ChatRequest {
        message: "hi".into(),
        session,
        incognito,
        background,
        profile: profile.map(str::to_string),
        model: None,
    }
}

fn finance_read() -> Capability {
    Capability::Read(Zone::vault("finance"))
}

async fn post_chat(state: Arc<AppState>, req: ChatRequest) -> (StatusCode, String) {
    let response = chat(State(state), Json(req)).await.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn sse_text(response: axum::response::Response) -> String {
    use futures::StreamExt;
    let mut body = Box::pin(response.into_body().into_data_stream());
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(200), body.as_mut().next())
            .await
        {
            Ok(Some(Ok(frame))) => seen.push_str(&String::from_utf8_lossy(&frame)),
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => {
                if seen.contains("event: failed") {
                    break;
                }
            }
        }
    }
    seen
}

fn failed_message(body: &str) -> String {
    let mut want = false;
    for line in body.lines() {
        let line = line.trim();
        if line == "event: failed" || line == "event:failed" {
            want = true;
            continue;
        }
        if want && let Some(data) = line.strip_prefix("data:") {
            let value: serde_json::Value = serde_json::from_str(data.trim()).unwrap();
            return value["message"].as_str().unwrap().to_string();
        }
        if line.is_empty() {
            want = false;
        }
    }
    panic!("no failed event in:\n{body}");
}

fn session_id(body: &str) -> Ulid {
    let value: serde_json::Value = serde_json::from_str(body).unwrap();
    value["session"].as_str().unwrap().parse().unwrap()
}

/// A named profile on the first message is the grant the session stores, including the
/// capability the profile declares. The default grant does not have that read.
#[tokio::test]
async fn post_chat_with_a_profile_stores_that_grant() {
    let h = harness().await;
    let (status, body) = post_chat(
        h.state.clone(),
        request(None, false, false, Some("budget-bot")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = session_id(&body);
    let header = h.state.sessions.session(id).await.unwrap();
    assert_eq!(header.grant.profile.as_deref(), Some("budget-bot"));
    assert_eq!(header.grant.model.as_deref(), Some("budget-small"));
    assert_eq!(
        header.grant.prompt_append.as_deref(),
        Some("Stay inside the budget.")
    );
    assert!(header.grant.capabilities.contains(&finance_read()));
    assert!(!header.ephemeral);
    assert_eq!(header.visibility, Visibility::Foreground);
    let sidebar = h.state.chat.as_ref().unwrap().list().await.unwrap();
    assert!(sidebar.iter().any(|row| row.id == id));
}

/// The control for the test above. With no profile, the same daemon must not borrow
/// `budget-bot`'s read just because that profile exists.
#[tokio::test]
async fn post_chat_without_a_profile_keeps_the_default_grant() {
    let h = harness().await;
    let (status, body) = post_chat(h.state.clone(), request(None, false, false, None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let header = h.state.sessions.session(session_id(&body)).await.unwrap();
    assert!(header.grant.profile.is_none());
    assert!(header.grant.model.is_none());
    assert!(!header.grant.capabilities.contains(&finance_read()));
    assert_eq!(header.visibility, Visibility::Foreground);
}

/// Background is durable and out of the sidebar, and it keeps the profile grant.
#[tokio::test]
async fn post_chat_background_keeps_the_grant_and_leaves_the_sidebar() {
    let h = harness().await;
    let (status, body) = post_chat(
        h.state.clone(),
        request(None, false, true, Some("budget-bot")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = session_id(&body);
    let header = h.state.sessions.session(id).await.unwrap();
    assert_eq!(header.visibility, Visibility::Background);
    assert!(!header.ephemeral);
    assert_eq!(header.grant.profile.as_deref(), Some("budget-bot"));
    assert!(header.grant.capabilities.contains(&finance_read()));
    let sidebar = h.state.chat.as_ref().unwrap().list().await.unwrap();
    assert!(sidebar.iter().all(|row| row.id != id));
    let durable = h.state.sessions.list_sessions().await;
    assert!(durable.iter().any(|row| row.id == id));
}

/// Incognito wins when background and a profile are also set. The transcript is RAM-only,
/// and the profile grant is not applied: that is what the stream's creation order does.
#[tokio::test]
async fn post_chat_incognito_wins_and_drops_the_grant() {
    let h = harness().await;
    let (status, body) = post_chat(
        h.state.clone(),
        request(None, true, true, Some("budget-bot")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = session_id(&body);
    let header = h.state.sessions.session(id).await.unwrap();
    assert!(header.ephemeral);
    assert_eq!(header.visibility, Visibility::Foreground);
    assert!(header.grant.profile.is_none());
    assert!(!header.grant.capabilities.contains(&finance_read()));
    assert!(h.state.sessions.list_sessions().await.is_empty());
    let sidebar = h.state.chat.as_ref().unwrap().list().await.unwrap();
    assert!(sidebar.is_empty());
}

async fn assert_refused(state: &Arc<AppState>, session: Option<Ulid>, name: &str) {
    let expected = resolve_chat_grant(&state.surfaces.profiles, Some(name)).unwrap_err();
    let before = state.sessions.list_sessions().await.len();

    let (status, body) = post_chat(
        Arc::clone(state),
        request(session, false, false, Some(name)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["error"].as_str(), Some(expected.as_str()));

    let stream = chat_stream_post(
        State(Arc::clone(state)),
        Json(request(session, false, false, Some(name))),
    )
    .await;
    assert_eq!(stream.status(), StatusCode::OK);
    let sse = sse_text(stream).await;
    assert_eq!(failed_message(&sse), expected);
    assert_eq!(state.sessions.list_sessions().await.len(), before);
}

/// An unknown name and a disabled profile fail closed. The JSON error and the stream's
/// `failed` event carry the same text, and neither call creates a conversation.
#[tokio::test]
async fn an_unknown_or_disabled_profile_is_refused_like_the_stream() {
    let h = harness().await;
    assert_refused(&h.state, None, "nosuch").await;
    assert_refused(&h.state, None, "retired").await;
    assert!(h.state.sessions.list_sessions().await.is_empty());
}

/// Flags on a later message describe how to *open* a conversation. They do not retarget
/// the one the client already named, and a bad profile still fails before the turn.
#[tokio::test]
async fn creation_flags_on_a_continuation_do_not_retarget_or_slip_through() {
    let h = harness().await;
    let id = h.state.chat.as_ref().unwrap().create(None).await.unwrap();

    let (status, body) = post_chat(
        h.state.clone(),
        request(Some(id), true, true, Some("budget-bot")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let kept = h.state.sessions.session(id).await.unwrap();
    assert!(!kept.ephemeral);
    assert_eq!(kept.visibility, Visibility::Foreground);
    assert!(kept.grant.profile.is_none());
    assert!(!kept.grant.capabilities.contains(&finance_read()));

    assert_refused(&h.state, Some(id), "nosuch").await;
    let still = h.state.sessions.session(id).await.unwrap();
    assert!(still.grant.profile.is_none());
    assert_eq!(h.state.sessions.list_sessions().await.len(), 1);
    let nodes = ConversationStore::leaf_path(h.state.sessions.as_ref(), id, None)
        .await
        .unwrap();
    let user_turns = nodes
        .iter()
        .filter(|node| matches!(node.author, liberado_conversation_store::Author::User))
        .count();
    assert_eq!(
        user_turns, 1,
        "the refused continuation must not append another user turn"
    );
}
