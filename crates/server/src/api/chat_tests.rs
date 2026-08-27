//! Split from `chat.rs` for module-health boundaries.

use super::ChatRequest;
use std::str::FromStr;

/// The WebUI reaches the chat stream through `EventSource`, which can only issue a `GET` — so
/// `incognito` crosses the wire as a query parameter, and axum deserializes it with
/// `serde_urlencoded`. That deserializer parses a `bool` through `FromStr`, which accepts
/// **only** `true`/`false`.
///
/// This is pinned because the failure is disproportionate to the mistake: `incognito=1` does not
/// fall back to `false`, it fails the whole `Query` extraction, so the request 400s and the chat
/// just does not answer. Nothing in the type signature hints at that.
#[test]
fn incognito_parses_from_the_query_string_as_true_not_one() {
    let ok: ChatRequest = serde_urlencoded::from_str("message=hi&incognito=true").unwrap();
    assert!(ok.incognito);

    let off: ChatRequest = serde_urlencoded::from_str("message=hi&incognito=false").unwrap();
    assert!(!off.incognito);

    // Absent is the overwhelmingly common case: every normal chat, and every other client.
    let absent: ChatRequest = serde_urlencoded::from_str("message=hi").unwrap();
    assert!(!absent.incognito);

    assert!(
        serde_urlencoded::from_str::<ChatRequest>("message=hi&incognito=1").is_err(),
        "if `1` ever starts parsing, the comment on the URL builder in webui/chat.rs is stale"
    );
}

/// The WebUI error bubble renders this string as-is from the SSE `failed` payload. UTF-8
/// em-dash bytes (`E2 80 94`) misread as Windows-1252 become U+00E2 U+20AC U+201D (`â€"`).
#[test]
fn chat_disabled_hint_is_a_utf8_em_dash_not_windows1252_mojibake() {
    let hint = super::CHAT_DISABLED_HINT;
    assert_eq!(
        hint.as_bytes(),
        b"chat is disabled \xe2\x80\x94 set DEEPSEEK_API_KEY",
        "{hint:?}"
    );
    assert!(
        hint.contains('\u{2014}'),
        "expected U+2014 em-dash, got {hint:?}"
    );
    assert!(
        !hint.contains('\u{00e2}'),
        "U+00E2 is the first character of the Windows-1252 misread of U+2014: {hint:?}"
    );

    // Same JSON-in-SSE path the WebUI `failed` listener runs (`from_sse_data`).
    let json = serde_json::json!({ "message": hint }).to_string();
    let event = chat_client_contract::SessionEvent::from_sse_data("failed", &json)
        .expect("disabled-chat payload must decode");
    match event.kind {
        chat_client_contract::SessionEventKind::Failed { message } => {
            assert_eq!(message, hint);
            assert!(message.contains('\u{2014}'), "{message:?}");
            assert!(!message.contains('\u{00e2}'), "{message:?}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

// ── The `?ephemeral_only=true` guard ─────────────────────────────────────────────────────
//
// Driven through the real router and the real store, because what is being asserted is that a
// *request* cannot destroy data — and the parts that would let it (route wiring, extractor
// order, the store's own notion of ephemerality) are precisely the parts a narrower test would
// stub out.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use liberado_conversation_store::ConversationStore;
use liberado_executor::{Budget, Executor};
use liberado_main_agent::ChatSessions;
use liberado_provider::MockProvider;
use liberado_session_store::SessionStore;
use liberado_test_support::NoopRuntime;
use tower::ServiceExt;

use super::*;

struct Harness {
    app: Router,
    chat: Arc<ChatSessions>,
    sessions: Arc<SessionStore>,
    state: Arc<crate::state::AppState>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    harness_scripted(vec![]).await
}

/// A harness whose provider will actually answer, for the tests that need a turn to complete.
async fn harness_scripted(script: Vec<liberado_provider::CompletionResponse>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(dir.path()).await);
    let executor = Executor::new(
        Arc::new(MockProvider::with_script("mock", script)),
        Budget::default(),
    );
    let chat = Arc::new(ChatSessions::new(
        sessions.clone(),
        executor,
        Arc::new(NoopRuntime),
    ));
    let state = Arc::new(AppState::for_test(
        sessions.clone(),
        Some(chat.clone()),
        dir.path().to_path_buf(),
    ));
    let app = Router::new()
        .route(
            "/api/conversations/{id}",
            axum::routing::delete(super::delete_conversation),
        )
        .route("/api/chat", axum::routing::post(super::chat))
        .with_state(state.clone());
    Harness {
        app,
        chat,
        sessions,
        state,
        _dir: dir,
    }
}

async fn delete(app: &Router, uri: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// The heartbeat, on the real response the handler returns.
///
/// Asserted because its absence is invisible: a stream with no keep-alive looks identical to one
/// with a keep-alive that has simply not ticked yet, and the symptom shows up minutes later as a
/// dropped connection on someone else's machine. On 2026-08-01 a delegated turn sent nothing for
/// 3m28s and the connection died before the answer arrived.
///
/// `start_paused` runs tokio's clock on demand: the stream below never yields, so time advances
/// straight to the heartbeat's timer and the test finishes instantly rather than waiting the real
/// 15 seconds.
#[tokio::test(start_paused = true)]
async fn an_idle_stream_still_sends_a_heartbeat() {
    use http_body_util::BodyExt;

    let idle = Box::pin(futures::stream::pending::<Result<Event, Infallible>>()) as SseBody;
    let response = Sse::new(idle)
        .keep_alive(super::keep_alive())
        .into_response();

    // Bounded, so losing the keep-alive fails this test instead of hanging it. Without a
    // heartbeat the body never yields, and an unbounded await would block CI rather than report.
    // The bound is virtual time too, and later than the heartbeat, so the heartbeat still wins.
    let frame = tokio::time::timeout(super::KEEP_ALIVE_INTERVAL * 4, response.into_body().frame())
        .await
        .expect("no heartbeat arrived: an idle stream produced nothing before the deadline")
        .expect("an idle stream must still produce a frame")
        .expect("the heartbeat frame must not be an error");
    let bytes = frame.into_data().expect("a data frame");

    // An SSE comment: the wire form every client already ignores.
    assert_eq!(
        &bytes[..],
        b":

",
        "expected a keep-alive comment, got {:?}",
        String::from_utf8_lossy(&bytes)
    );
}

/// The interval is a decision, not a literal. Too long and it stops clearing the idle timeouts it
/// exists for — proxies commonly close at 60s — and nothing else in the system would notice.
#[test]
fn the_heartbeat_stays_inside_common_idle_timeouts() {
    assert!(
        super::KEEP_ALIVE_INTERVAL >= std::time::Duration::from_secs(1)
            && super::KEEP_ALIVE_INTERVAL <= std::time::Duration::from_secs(30),
        "keep-alive interval {:?} is outside the range that clears a 60s proxy timeout",
        super::KEEP_ALIVE_INTERVAL
    );
}

// ── `model` on the request ───────────────────────────────────────────────────────────────

/// A model asked for on the message must reach the turn — asserted on the **stamp the turn
/// left**, not on the request parsing, because parsing correctly and then being ignored is
/// exactly how this failed live: the field existed nowhere, the picker fell back to the
/// daemon-wide swap, and every other conversation moved with it.
#[tokio::test]
async fn a_model_on_the_request_is_the_model_the_turn_runs_on() {
    let h = harness_scripted(vec![liberado_provider::CompletionResponse::text("ok")]).await;
    let id = h.chat.create(None).await.unwrap();

    let resp = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"message":"hi","session":"{id}","model":"picked/one"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let nodes = h.sessions.leaf_path(id, None).await.unwrap();
    let user = nodes
        .iter()
        .find(|n| matches!(n.author, liberado_conversation_store::Author::User))
        .expect("the user message is persisted before inference");
    assert_eq!(
        user.model.as_deref(),
        Some("picked/one"),
        "the turn ran on the daemon default instead of the model the request asked for"
    );
}

/// The positive control for the test above. Without it, a handler that stamped every turn with
/// some fixed string would pass — and so would one that ignored `model` while the default
/// happened to match it, which is precisely the confound that made the first live test of this
/// feature unreadable.
#[tokio::test]
async fn no_model_on_the_request_leaves_the_default_alone() {
    let h = harness_scripted(vec![liberado_provider::CompletionResponse::text("ok")]).await;
    let id = h.chat.create(None).await.unwrap();

    let resp = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"message":"hi","session":"{id}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let nodes = h.sessions.leaf_path(id, None).await.unwrap();
    let user = nodes
        .iter()
        .find(|n| matches!(n.author, liberado_conversation_store::Author::User))
        .unwrap();
    assert_eq!(
        user.model.as_deref(),
        Some("mock"),
        "with nothing asked for, the turn takes the provider's own model"
    );
}

#[test]
fn chat_message_from_node_carries_the_store_model_stamp() {
    use liberado_conversation_store::{Author, MessageNode, Ulid};
    use liberado_provider::{Message, Role};

    let node = MessageNode {
        id: Ulid::new(),
        parent_id: None,
        conversation_id: Ulid::new(),
        author: Author::Assistant,
        created_at: chrono::Utc::now(),
        message: Message {
            role: Role::Assistant,
            content: "hi".into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        model: Some("vendor/slug".into()),
    };
    let wire = chat_message_from_node(node);
    assert_eq!(wire.role, "assistant");
    assert_eq!(wire.model.as_deref(), Some("vendor/slug"));
}

/// Both transports, because `EventSource` can only `GET` and so the WebUI's picks arrive as a
/// query parameter while every other client sends JSON. A field that works on one of the two is
/// worse than one that works on neither, because it looks fine wherever you happen to test it.
#[test]
fn model_parses_from_both_the_query_string_and_json() {
    let q: ChatRequest = serde_urlencoded::from_str("message=hi&model=vendor%2Fslug").unwrap();
    assert_eq!(q.model.as_deref(), Some("vendor/slug"));

    let j: ChatRequest = serde_json::from_str(r#"{"message":"hi","model":"vendor/slug"}"#).unwrap();
    assert_eq!(j.model.as_deref(), Some("vendor/slug"));

    let absent: ChatRequest = serde_urlencoded::from_str("message=hi").unwrap();
    assert!(absent.model.is_none());
}

/// Deleting a conversation must stop the turn running in it first.
///
/// This became load-bearing when turns started outliving their connection. The incognito
/// teardown fires when the human navigates away — precisely when a turn they left behind is
/// still going — so "delete while a turn is running" went from a narrow race to the ordinary
/// case. Without the cancel, a detached turn keeps writing to a conversation that has been
/// removed underneath it.
#[tokio::test]
async fn deleting_a_conversation_stops_its_running_turn_first() {
    let h = harness_scripted(vec![]).await;
    let ghost = h.chat.create_incognito(None).await.unwrap();

    // A turn that will not finish on its own: the scripted provider has no reply queued.
    let (_replay, _rx) = h.chat.start_or_attach(ghost, "still thinking");
    assert!(
        h.chat.turn_running(ghost) || h.chat.history(ghost).await.is_ok(),
        "precondition: the turn was registered"
    );

    let status = delete(
        &h.app,
        &format!("/api/conversations/{ghost}?ephemeral_only=true"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(
        !h.chat.turn_running(ghost),
        "the turn must be cancelled by the delete, not left writing to a removed conversation"
    );
}

/// The guard that turns "a client sent the wrong id" from permanent data loss into a no-op.
///
/// This is not hypothetical. The WebUI's incognito teardown once mistook a saved conversation
/// for its private session and deleted it — no confirmation, no undo, no backup. Every automatic
/// teardown now passes this flag, so the same class of bug can only fail to clean up.
#[tokio::test]
async fn ephemeral_only_delete_refuses_a_durable_conversation() {
    let h = harness().await;
    let durable = h.chat.create(None).await.unwrap();

    let status = delete(
        &h.app,
        &format!("/api/conversations/{durable}?ephemeral_only=true"),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        h.chat.history(durable).await.is_ok(),
        "the conversation must still be there — a refused delete that deleted anyway is the \
             whole bug this guards"
    );
}

#[tokio::test]
async fn ephemeral_only_delete_removes_an_incognito_session() {
    let h = harness().await;
    let ghost = h.chat.create_incognito(None).await.unwrap();

    let status = delete(
        &h.app,
        &format!("/api/conversations/{ghost}?ephemeral_only=true"),
    )
    .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(h.chat.history(ghost).await.is_err());
}

// ── Profile switching ────────────────────────────────────────────────────────────────────

async fn post_profile(app: &Router, id: &str, body: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/conversations/{id}/profile"))
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

fn profile_router(state: Arc<crate::state::AppState>) -> Router {
    Router::new()
        .route(
            "/api/conversations/{id}/profile",
            axum::routing::post(super::set_conversation_profile),
        )
        .with_state(state)
}

/// A typo must not resolve to "no profile", which would silently mean the *default* grant — a
/// wider one than the profile being asked for. `resolve_session_profile` fails closed and this
/// is the endpoint honouring that rather than swallowing it.
#[tokio::test]
async fn an_unknown_profile_is_refused_and_the_grant_is_untouched() {
    let h = harness().await;
    let id = h.chat.create(None).await.unwrap();
    let app = profile_router(h.state.clone());

    let (status, body) = post_profile(&app, &id.to_string(), r#"{"name":"nonesuch"}"#).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("nonesuch"), "the error must name it: {body}");
    assert!(
        h.sessions
            .session(id)
            .await
            .expect("still there")
            .grant
            .profile
            .is_none(),
        "a refused switch must leave the conversation on the grant it had"
    );
}

#[tokio::test]
async fn switching_to_an_unknown_conversation_is_404() {
    let h = harness().await;
    let app = profile_router(h.state.clone());
    let (status, _) = post_profile(&app, &Ulid::new().to_string(), r#"{"name":null}"#).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Clearing is a real choice, not an error: it returns a chat to the daemon's default grant,
/// which is what every conversation ran under before profiles existed.
#[tokio::test]
async fn clearing_the_profile_is_allowed_and_records_a_note() {
    let h = harness().await;
    let id = h.chat.create(None).await.unwrap();
    let app = profile_router(h.state.clone());

    let (status, body) = post_profile(&app, &id.to_string(), r#"{"name":null}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("next turn"), "say when it applies: {body}");

    // The switch is recorded in the transcript, not only in the header — a change of authority
    // the human cannot see in the thread is not meaningfully recorded.
    let nodes =
        liberado_conversation_store::ConversationStore::leaf_path(h.sessions.as_ref(), id, None)
            .await
            .unwrap();
    assert!(
        nodes.iter().any(|n| matches!(
            &n.author,
            liberado_conversation_store::Author::Named(name)
                if name == liberado_main_agent::PROFILE_AUTHOR
        )),
        "a profile-authored note must be on the transcript"
    );
}

/// Without the flag the endpoint is unchanged — that is the path the sidebar's Delete button
/// takes, where a human clicked and confirmed.
#[tokio::test]
async fn an_unguarded_delete_still_removes_a_durable_conversation() {
    let h = harness().await;
    let durable = h.chat.create(None).await.unwrap();

    let status = delete(&h.app, &format!("/api/conversations/{durable}")).await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(h.chat.history(durable).await.is_err());
}

// ── chat-stream session resolution (resolve_chat_grant, pick_chat_creation) ──────────────

#[test]
fn pick_chat_creation_prefers_incognito_then_background_then_grant() {
    let grant = liberado_session::SessionGrant {
        profile: Some("research".into()),
        ..liberado_session::SessionGrant::default()
    };

    // Incognito wins over background when both are set.
    assert!(matches!(
        pick_chat_creation(true, true, None),
        ChatCreation::Incognito
    ));
    // Background without a grant falls back to an empty grant.
    assert!(matches!(
        pick_chat_creation(false, true, None),
        ChatCreation::Background(g) if g.profile.is_none()
    ));
    // A present grant routes through the profile-aware constructor.
    assert!(matches!(
        pick_chat_creation(false, false, Some(grant.clone())),
        ChatCreation::Granted(g) if g.profile.as_deref() == Some("research")
    ));
    assert!(matches!(
        pick_chat_creation(false, false, None),
        ChatCreation::Default
    ));
}

#[test]
fn resolve_chat_grant_returns_none_for_no_profile() {
    let config = liberado_bootstrap::Config::default();
    assert!(resolve_chat_grant(&config, None).unwrap().is_none());
}

#[test]
fn resolve_chat_grant_fails_closed_on_unknown_profile() {
    let config = liberado_bootstrap::Config::from_str(
        r#"
[topology]
vault_path = "/tmp/vault"

[[topology.session_profiles]]
name = "research"

[[policy.grants]]
component = "research"
capabilities = []
"#,
    )
    .unwrap();
    // A named enabled profile resolves to a grant carrying its name.
    let Some(grant) = resolve_chat_grant(&config, Some("research")).unwrap() else {
        panic!("enabled profile must resolve");
    };
    assert_eq!(grant.profile.as_deref(), Some("research"));
    // Unknown name fails closed, naming the profile rather than silently downgrading.
    let err = resolve_chat_grant(&config, Some("nosuch")).unwrap_err();
    assert!(err.contains("nosuch"), "error must name the profile: {err}");
}
